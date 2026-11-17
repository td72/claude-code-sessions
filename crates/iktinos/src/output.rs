//! The whole output of one tool call, read again from the transcript.
//!
//! A [`Session`](crate::session::Session) keeps at most an excerpt of what a
//! tool returned ([`ToolCall::output`](crate::session::ToolCall::output),
//! and only for a store that asked for it). The whole text is not kept
//! anywhere: [`read_tool_output`] reads it from the session's files when
//! someone wants to see it, and hands it to the caller.
//!
//! The call is found by its `tool_use_id`, by reading the session's
//! transcripts (the main one, then its subagents') until the result turns
//! up. That works for a call the session no longer lists, and costs one
//! pass over the files. An output Claude Code moved out of the transcript
//! (`<session>/tool-results/<tool_use_id>.txt`) is read from that file.

use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::discover::SessionFiles;
use crate::record::output_parts;

/// The most of one output that is read, in bytes. What is longer is cut,
/// and [`WholeOutput::truncated`] says so.
pub const MAX_OUTPUT_BYTES: usize = 32 * 1024 * 1024;

/// Everything one tool call returned.
#[derive(Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct WholeOutput {
    /// The text: the `content` of the `tool_result` block (a string, or its
    /// text blocks joined), else `toolUseResult`'s `stdout`; or the file
    /// the output was moved to.
    pub text: String,
    /// `toolUseResult`'s `stderr`, where it holds anything.
    pub stderr: Option<String>,
    /// The tool was interrupted.
    pub interrupted: bool,
    /// Blocks that are not text (an image, …): how many there were.
    pub other_blocks: usize,
    /// The text or the stderr was longer than [`MAX_OUTPUT_BYTES`] and is
    /// cut here.
    pub truncated: bool,
    /// The file the text was read from, when it is not the transcript.
    pub file: Option<PathBuf>,
}

/// `Debug` says how much there is, not what.
impl fmt::Debug for WholeOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WholeOutput")
            .field("bytes", &self.text.len())
            .field("stderr", &self.stderr.is_some())
            .field("interrupted", &self.interrupted)
            .field("other_blocks", &self.other_blocks)
            .field("truncated", &self.truncated)
            .field("file", &self.file.is_some())
            .finish_non_exhaustive()
    }
}

/// Reads the whole output of the call `tool_use_id` of the session `files`
/// describes. `Ok(None)` when no result of that call is in the session's
/// files: it is still running, or it is another session's.
///
/// Nothing is kept: every call reads the files again.
pub fn read_tool_output(
    files: &SessionFiles,
    tool_use_id: &str,
) -> io::Result<Option<WholeOutput>> {
    if tool_use_id.is_empty() {
        return Ok(None);
    }
    let transcripts = std::iter::once(&files.transcript.path).chain(
        files
            .subagents
            .iter()
            .map(|subagent| &subagent.transcript.path),
    );
    let mut found = None;
    for path in transcripts {
        match scan(path, tool_use_id) {
            Ok(Some(output)) => {
                found = Some(output);
                break;
            }
            Ok(None) => {}
            // A transcript that went away since it was listed has nothing.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    // An output too large for the transcript is in a file of its own.
    if let Some(path) = moved_to(files, tool_use_id) {
        match read_capped(&path) {
            Ok((text, truncated)) => {
                let mut output = found.unwrap_or_default();
                output.text = text;
                output.truncated |= truncated;
                output.file = Some(path);
                return Ok(Some(output));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(found)
}

/// Where Claude Code puts an output it moved out of the transcript. `None`
/// for an id that is not a plain file name.
fn moved_to(files: &SessionFiles, tool_use_id: &str) -> Option<PathBuf> {
    let plain = tool_use_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    plain.then(|| {
        files
            .transcript
            .path
            .with_extension("")
            .join("tool-results")
            .join(format!("{tool_use_id}.txt"))
    })
}

/// The result of the call in one transcript, if it is there.
fn scan(path: &Path, tool_use_id: &str) -> io::Result<Option<WholeOutput>> {
    let mut reader = BufReader::new(File::open(path)?);
    let needle = tool_use_id.as_bytes();
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(None);
        }
        // Most lines do not mention the call: those are not parsed.
        if !line.windows(needle.len()).any(|window| window == needle) {
            continue;
        }
        let Ok(Value::Object(record)) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if record.get("type").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let blocks = record
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array);
        let results: Vec<_> = blocks
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
            .collect();
        let Some(block) = results
            .iter()
            .find(|block| block.get("tool_use_id").and_then(Value::as_str) == Some(tool_use_id))
        else {
            continue;
        };
        let extra = record.get("toolUseResult").filter(|_| results.len() == 1);
        let parts = output_parts(block, extra);
        let (text, text_cut) = capped(&parts.text);
        let stderr = parts
            .stderr
            .filter(|stderr| !stderr.trim().is_empty())
            .map(capped);
        return Ok(Some(WholeOutput {
            truncated: text_cut || stderr.as_ref().is_some_and(|(_, cut)| *cut),
            text,
            stderr: stderr.map(|(stderr, _)| stderr),
            interrupted: parts.interrupted,
            other_blocks: parts.other_blocks,
            file: None,
        }));
    }
}

/// `text` up to [`MAX_OUTPUT_BYTES`], cut between characters, and whether it
/// was cut.
fn capped(text: &str) -> (String, bool) {
    if text.len() <= MAX_OUTPUT_BYTES {
        return (text.to_owned(), false);
    }
    let mut end = MAX_OUTPUT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

/// A file's text up to [`MAX_OUTPUT_BYTES`], and whether it was cut. Bytes
/// that are not UTF-8 are replaced.
fn read_capped(path: &Path) -> io::Result<(String, bool)> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_OUTPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > MAX_OUTPUT_BYTES;
    bytes.truncate(MAX_OUTPUT_BYTES);
    Ok((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}
