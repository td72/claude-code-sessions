//! Offset-based line reading.
//!
//! A [`Tail`] remembers how far into a transcript it has read. Each
//! [`poll`](Tail::poll) returns the complete lines appended since the last
//! one, with the offset each line ends at, and nothing else is read again.
//!
//! - A partial last line (Claude Code is in the middle of writing it) is held
//!   back until its newline arrives.
//! - A file that shrank, or was replaced by another file, is read again from
//!   the beginning, and the poll says so.
//! - [`Tail::from_end`] starts a bounded number of bytes before the end of the
//!   file instead of at its beginning, for a first look at a multi-megabyte
//!   transcript.
//!
//! The caller decides when to poll. With the `watch` feature,
//! [`watch::Watcher`] reports which transcripts changed, so a caller polls
//! only those.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::record::Record;

#[cfg(feature = "watch")]
pub mod watch;

/// A reading position in one transcript.
#[derive(Debug, Clone)]
pub struct Tail {
    path: PathBuf,
    /// The first byte not yet returned. Always the start of a line, except
    /// while `align` is pending.
    offset: u64,
    /// Set by [`Tail::from_end`]: the first poll starts inside a line and
    /// discards up to the next line start.
    align: Option<u64>,
    identity: Option<FileIdentity>,
}

impl Tail {
    /// Reads from the beginning of the file.
    pub fn from_start(path: impl Into<PathBuf>) -> Self {
        Self::at(path, 0)
    }

    /// Reads from `offset`, which must be the end offset of a line a previous
    /// tail returned (or 0).
    pub fn at(path: impl Into<PathBuf>, offset: u64) -> Self {
        Self {
            path: path.into(),
            offset,
            align: None,
            identity: None,
        }
    }

    /// Reads the lines that start within the last `window` bytes of the file,
    /// then follows appends. The first poll reads at most `window` bytes plus
    /// one.
    ///
    /// If a single line is longer than the window, the first poll returns
    /// nothing and reading continues from the end of the file as it was.
    pub fn from_end(path: impl Into<PathBuf>, window: u64) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            align: Some(window),
            identity: None,
        }
    }

    /// The file this tail reads.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The offset reading will continue from. Persist it to resume with
    /// [`Tail::at`].
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Returns the complete lines appended since the last poll.
    pub fn poll(&mut self) -> io::Result<Batch> {
        let mut lines = Vec::new();
        let polled = self.poll_each(|bytes, end_offset| {
            lines.push(Line {
                bytes: bytes.to_vec(),
                end_offset,
            });
        })?;
        Ok(Batch {
            lines,
            restarted: polled.restarted,
            bytes_read: polled.bytes_read,
        })
    }

    /// Like [`poll`](Tail::poll), but hands each line to `line` as it is read
    /// instead of collecting them: the way to read a large file without
    /// holding it in memory. The arguments are the line without its newline
    /// and the offset it ends at.
    pub fn poll_each(&mut self, mut line: impl FnMut(&[u8], u64)) -> io::Result<Polled> {
        let mut file = File::open(&self.path)?;
        let meta = file.metadata()?;
        let len = meta.len();
        let identity = FileIdentity::of(&meta);

        let replaced = match (self.identity, identity) {
            (Some(before), Some(now)) => before != now,
            _ => false,
        };
        let restarted = replaced || len < self.offset;
        if restarted {
            self.offset = 0;
            self.align = None;
        }
        self.identity = identity;

        // A window that reaches the start of the file needs no alignment.
        let mut discard_first = false;
        if let Some(window) = self.align.take() {
            if len > window {
                // One byte early: if that byte is a newline the window starts
                // exactly on a line, and discarding "up to the first newline"
                // discards only it.
                self.offset = len - window - 1;
                discard_first = true;
            }
        }

        let mut polled = Polled {
            lines: 0,
            restarted,
            bytes_read: 0,
        };
        if self.offset >= len {
            return Ok(polled);
        }

        file.seek(SeekFrom::Start(self.offset))?;
        // Read no further than the length seen above, so one poll is bounded
        // even while the file keeps growing.
        let mut reader = BufReader::new(file.take(len - self.offset));
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let n = reader.read_until(b'\n', &mut buf)? as u64;
            if n == 0 {
                break;
            }
            polled.bytes_read += n;
            if buf.last() != Some(&b'\n') {
                if discard_first {
                    // The whole window was the inside of one line.
                    self.offset += n;
                }
                // Otherwise a partial line: leave the offset at its start.
                break;
            }
            self.offset += n;
            if discard_first {
                discard_first = false;
                continue;
            }
            let content = buf.trim_ascii();
            if !content.is_empty() {
                polled.lines += 1;
                line(content, self.offset);
            }
        }
        Ok(polled)
    }
}

/// What one [`Tail::poll`] returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    /// The new complete lines, oldest first. Blank lines are left out.
    pub lines: Vec<Line>,
    /// The file shrank or was replaced, and was read again from its
    /// beginning: whatever was folded from it before must be discarded.
    pub restarted: bool,
    /// Bytes read from the file by this poll.
    pub bytes_read: u64,
}

/// What one [`Tail::poll_each`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Polled {
    /// Lines handed to the callback.
    pub lines: usize,
    /// See [`Batch::restarted`].
    pub restarted: bool,
    /// Bytes read from the file by this poll.
    pub bytes_read: u64,
}

/// One complete line of a transcript.
#[derive(Clone, PartialEq, Eq)]
pub struct Line {
    /// The line without its newline.
    pub bytes: Vec<u8>,
    /// The offset just past the line's newline: where the next line starts.
    pub end_offset: u64,
}

impl Line {
    /// Parses the line as a transcript record.
    pub fn record(&self) -> Record {
        Record::parse_bytes(&self.bytes)
    }
}

/// `Debug` leaves the content out: a line is a piece of a conversation.
impl std::fmt::Debug for Line {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Line")
            .field("len", &self.bytes.len())
            .field("end_offset", &self.end_offset)
            .finish()
    }
}

/// Which file a path pointed at, to notice a replacement that kept the size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    #[cfg(unix)]
    fn of(meta: &fs::Metadata) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        Some(Self {
            device: meta.dev(),
            inode: meta.ino(),
        })
    }

    /// Elsewhere only a shrinking file is noticed.
    #[cfg(not(unix))]
    fn of(_meta: &fs::Metadata) -> Option<Self> {
        None
    }
}
