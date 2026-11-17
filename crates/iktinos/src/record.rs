//! Typed views of transcript lines.
//!
//! One line of a transcript is one [`Record`]: the raw JSON value, the fields
//! every conversational record shares ([`Common`]) and a typed [`View`] of the
//! handful of record types the session model reads. Parsing never fails: a
//! line that is not a JSON object is [`View::Invalid`], a `type` this crate has
//! never seen is [`View::Unknown`], and a modelled record that lacks a key
//! simply has `None` there — the consumer skips what it cannot use.
//!
//! The views carry what the session model reads today: identifiers, numbers,
//! names, the title and the last prompt. Everything else (responses, tool
//! inputs and outputs) stays reachable through [`Record::raw`], which
//! [`Record`]'s `Debug` deliberately leaves out so that a stray `{:?}` never
//! writes a conversation to a log.
//!
//! [`drift`] surveys a corpus for the shapes these views do not know.

use std::fmt;
use std::path::PathBuf;

use serde_json::{Map, Value};

pub mod drift;

/// The Claude Code version the views and the known-shape tables in [`drift`]
/// were last checked against.
pub const KNOWN_CLAUDE_CODE_VERSION: &str = "2.1.325";

/// Record types that are known and deliberately not modelled. They parse to
/// [`View::Other`] and are not reported by the drift survey.
pub const UNMODELLED_TYPES: &[&str] = &[
    "api-request-blob",
    "api-request-shape",
    "atis-latch",
    "attachment",
    "bridge-session",
    "file-history-delta",
    "file-history-snapshot",
    "mode",
    "permission-mode",
    "pr-link",
    "queue-operation",
    "session-owner",
    "tools-used",
];

/// One transcript line.
#[derive(Clone, PartialEq)]
pub struct Record {
    raw: Value,
    common: Common,
    view: View,
}

impl Record {
    /// Parses one line. Never fails; see [`View::Invalid`].
    pub fn parse(line: &str) -> Self {
        match serde_json::from_str::<Value>(line) {
            Ok(raw) => Self::from_value(raw),
            Err(_) => Self::invalid(),
        }
    }

    /// Parses one line given as bytes (a line that is not UTF-8 is invalid).
    pub fn parse_bytes(line: &[u8]) -> Self {
        match serde_json::from_slice::<Value>(line) {
            Ok(raw) => Self::from_value(raw),
            Err(_) => Self::invalid(),
        }
    }

    /// Builds a record from an already parsed value.
    pub fn from_value(raw: Value) -> Self {
        let Some(obj) = raw.as_object() else {
            return Self {
                raw,
                common: Common::default(),
                view: View::Invalid,
            };
        };
        let common = Common::from_object(obj);
        let view = View::from_object(common.kind.as_deref(), obj);
        Self { raw, common, view }
    }

    fn invalid() -> Self {
        Self {
            raw: Value::Null,
            common: Common::default(),
            view: View::Invalid,
        }
    }

    /// The typed view.
    pub fn view(&self) -> &View {
        &self.view
    }

    /// The fields shared by conversational records. All `None` for an invalid
    /// line.
    pub fn common(&self) -> &Common {
        &self.common
    }

    /// The record's `type`, if it has one.
    pub fn kind(&self) -> Option<&str> {
        self.common.kind.as_deref()
    }

    /// The raw JSON value (`Null` for a line that did not parse). It contains
    /// everything Claude Code wrote, whole conversations included: read what
    /// you need from it, do not log it.
    pub fn raw(&self) -> &Value {
        &self.raw
    }
}

/// `Debug` shows the typed side only. The raw value holds the whole
/// conversation and must not reach a log through a stray `{:?}`.
impl fmt::Debug for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Record")
            .field("common", &self.common)
            .field("view", &self.view)
            .finish_non_exhaustive()
    }
}

/// Fields shared by the conversational records (`user`, `assistant`, `system`,
/// `attachment`). Bookkeeping records carry only some of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Common {
    /// `type`.
    pub kind: Option<String>,
    pub uuid: Option<String>,
    pub parent_uuid: Option<String>,
    /// `sessionId` (or the newer duplicate `session_id`).
    pub session_id: Option<String>,
    pub timestamp: Option<Timestamp>,
    pub cwd: Option<PathBuf>,
    pub git_branch: Option<String>,
    /// The Claude Code version that wrote the record.
    pub version: Option<String>,
    pub is_sidechain: Option<bool>,
    /// Set on records written by a subagent.
    pub agent_id: Option<String>,
}

impl Common {
    fn from_object(obj: &Map<String, Value>) -> Self {
        Self {
            kind: string(obj, "type"),
            uuid: string(obj, "uuid"),
            parent_uuid: string(obj, "parentUuid"),
            session_id: string(obj, "sessionId").or_else(|| string(obj, "session_id")),
            timestamp: obj
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(Timestamp::parse),
            cwd: string(obj, "cwd").map(PathBuf::from),
            git_branch: string(obj, "gitBranch"),
            version: string(obj, "version"),
            is_sidechain: obj.get("isSidechain").and_then(Value::as_bool),
            agent_id: string(obj, "agentId"),
        }
    }
}

/// What a record is, as far as the session model cares.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum View {
    User(User),
    Assistant(Assistant),
    System(System),
    /// `ai-title`, `custom-title` or the older `summary`.
    Title(Title),
    /// `last-prompt`: the prompt the user typed last.
    LastPrompt(LastPrompt),
    /// `api-request`: the bookkeeping record an assistant message's
    /// `requestRef` points at.
    ApiRequest(ApiRequest),
    /// `cost-state`: Claude Code's own running total, written when it exits.
    CostState(CostState),
    /// A known type this crate does not model ([`UNMODELLED_TYPES`]).
    Other,
    /// A JSON object whose `type` is missing or has never been seen. Kept for
    /// the drift survey.
    Unknown,
    /// Not a JSON object: a truncated line, garbage, a bare scalar.
    Invalid,
}

impl View {
    fn from_object(kind: Option<&str>, obj: &Map<String, Value>) -> Self {
        match kind {
            Some("user") => Self::User(User::from_object(obj)),
            Some("assistant") => Self::Assistant(Assistant::from_object(obj)),
            Some("system") => Self::System(System::from_object(obj)),
            Some("ai-title") => Self::Title(Title {
                title: string(obj, "aiTitle"),
                source: TitleSource::Ai,
            }),
            Some("custom-title") => Self::Title(Title {
                title: string(obj, "customTitle"),
                source: TitleSource::Custom,
            }),
            Some("summary") => Self::Title(Title {
                title: string(obj, "summary"),
                source: TitleSource::Summary,
            }),
            Some("last-prompt") => Self::LastPrompt(LastPrompt {
                text: string(obj, "lastPrompt"),
                leaf_uuid: string(obj, "leafUuid"),
            }),
            Some("api-request") => Self::ApiRequest(ApiRequest {
                id: string(obj, "id"),
                query_source: string(obj, "querySource"),
            }),
            Some("cost-state") => Self::CostState(CostState {
                total_cost_usd: obj.get("totalCostUSD").and_then(Value::as_f64),
            }),
            Some(k) if UNMODELLED_TYPES.contains(&k) => Self::Other,
            _ => Self::Unknown,
        }
    }
}

/// A `user` record: a prompt, or the results of tool calls.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct User {
    /// One entry per `tool_result` block in the message.
    pub tool_results: Vec<ToolResult>,
    /// Injected by Claude Code rather than typed by the user.
    pub is_meta: bool,
    /// The summary that replaces the conversation after a compaction.
    pub is_compact_summary: bool,
    /// `promptId`: the prompt this record belongs to. A prompt and the
    /// results of the tool calls made to answer it share one.
    pub prompt_id: Option<String>,
    /// What the user asked, on the main thread: the first line of the
    /// prompt, cut to [`PROMPT_CHARS`]; for a slash command, its name.
    /// `None` for anything else: tool results, what Claude Code injected, a
    /// subagent's instructions. See [`summarize_prompt`].
    pub prompt: Option<String>,
}

/// The longest a prompt's line is kept, in characters.
pub const PROMPT_CHARS: usize = 120;

/// One line saying what a prompt asks: its first line that holds text, cut
/// to [`PROMPT_CHARS`]. A slash command is recorded as markup; its name
/// (`/review`) stands for it. Any other text that starts with markup was
/// written by Claude Code, not typed, and gives `None`.
pub fn summarize_prompt(text: &str) -> Option<String> {
    let text = text.trim();
    if text.starts_with('<') {
        let name = text.split_once("<command-name>")?.1;
        let name = name.split_once("</command-name>")?.0.trim();
        return (!name.is_empty()).then(|| cut(name, PROMPT_CHARS));
    }
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    Some(cut(line, PROMPT_CHARS))
}

impl User {
    fn from_object(obj: &Map<String, Value>) -> Self {
        let tool_results = content_blocks(obj)
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            .collect::<Vec<_>>();
        // `toolUseResult` is the record's, so it is one result's only when
        // the record holds one.
        let extra = obj.get("toolUseResult").filter(|_| tool_results.len() == 1);
        let tool_results = tool_results
            .into_iter()
            .map(|b| ToolResult {
                tool_use_id: string(b, "tool_use_id"),
                is_error: b.get("is_error").and_then(Value::as_bool),
                output: ToolOutput::excerpt(&output_parts(b, extra), OUTPUT_CHARS),
            })
            .collect::<Vec<_>>();
        let is_meta = flag(obj, "isMeta");
        let is_compact_summary = flag(obj, "isCompactSummary");
        // A prompt is text on the main thread that answers no tool call.
        let typed = tool_results.is_empty()
            && !is_meta
            && !is_compact_summary
            && !flag(obj, "isSidechain")
            && !obj.contains_key("agentId");
        let content = obj.get("message").and_then(|m| m.get("content"));
        let text = match content {
            Some(Value::String(text)) => Some(text.as_str()),
            // With an attachment the prompt is the first text block.
            Some(Value::Array(_)) => content_blocks(obj)
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .find_map(|b| b.get("text").and_then(Value::as_str)),
            _ => None,
        };
        Self {
            prompt: text.filter(|_| typed).and_then(summarize_prompt),
            tool_results,
            is_meta,
            is_compact_summary,
            prompt_id: string(obj, "promptId"),
        }
    }
}

/// The outcome of one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    /// The `tool_use` block this answers.
    pub tool_use_id: Option<String>,
    /// `is_error` as written; often absent on success.
    pub is_error: Option<bool>,
    /// What the tool returned, cut to [`OUTPUT_CHARS`]. `None` for a result
    /// that says nothing.
    pub output: Option<ToolOutput>,
}

/// The most of a tool's output a view keeps, in characters (of its text,
/// and of its stderr).
pub const OUTPUT_CHARS: usize = 4000;

/// What a tool returned, as much of it as a view shows. The whole of it is
/// read again from the transcript when it is asked for
/// ([`read_tool_output`](crate::output::read_tool_output)).
#[derive(Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolOutput {
    /// The start of the text: the `content` of the `tool_result` block (a
    /// string, or its text blocks joined), else `toolUseResult`'s `stdout`.
    pub text: String,
    /// The start of `toolUseResult`'s `stderr`, where it holds anything.
    pub stderr: Option<String>,
    /// The text or the stderr is longer than what is kept here.
    pub truncated: bool,
    /// The characters and the lines of the whole text.
    pub chars: usize,
    pub lines: usize,
    /// The tool was interrupted.
    pub interrupted: bool,
    /// Blocks that are not text (an image, …): how many there were.
    pub other_blocks: usize,
}

/// `Debug` says how much there is, not what: a tool's output is the user's
/// data and must not reach a log through a stray `{:?}`.
impl fmt::Debug for ToolOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolOutput")
            .field("chars", &self.chars)
            .field("lines", &self.lines)
            .field("stderr", &self.stderr.is_some())
            .field("truncated", &self.truncated)
            .field("interrupted", &self.interrupted)
            .field("other_blocks", &self.other_blocks)
            .finish_non_exhaustive()
    }
}

impl ToolOutput {
    /// The first `max` characters of what a result holds. `None` when it
    /// holds nothing at all.
    pub(crate) fn excerpt(parts: &OutputParts, max: usize) -> Option<Self> {
        let stderr = parts.stderr.filter(|text| !text.trim().is_empty());
        if parts.text.is_empty()
            && stderr.is_none()
            && !parts.interrupted
            && parts.other_blocks == 0
        {
            return None;
        }
        let (text, text_cut) = first_chars(&parts.text, max);
        let stderr = stderr.map(|stderr| first_chars(stderr, max));
        Some(Self {
            truncated: text_cut || stderr.as_ref().is_some_and(|(_, cut)| *cut),
            text,
            stderr: stderr.map(|(stderr, _)| stderr),
            chars: parts.text.chars().count(),
            lines: parts.text.lines().count(),
            interrupted: parts.interrupted,
            other_blocks: parts.other_blocks,
        })
    }

    /// This output with at most `max` characters of its text and of its
    /// stderr.
    pub fn cut_to(&self, max: usize) -> Self {
        let (text, text_cut) = first_chars(&self.text, max);
        let stderr = self
            .stderr
            .as_deref()
            .map(|stderr| first_chars(stderr, max));
        Self {
            truncated: self.truncated || text_cut || stderr.as_ref().is_some_and(|(_, cut)| *cut),
            text,
            stderr: stderr.map(|(stderr, _)| stderr),
            ..self.clone()
        }
    }
}

/// The first `max` characters of `text`, and whether there were more.
fn first_chars(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((end, _)) => (text[..end].to_owned(), true),
        None => (text.to_owned(), false),
    }
}

/// Everything one tool result holds, uncut.
pub(crate) struct OutputParts<'a> {
    pub text: std::borrow::Cow<'a, str>,
    pub stderr: Option<&'a str>,
    pub interrupted: bool,
    pub other_blocks: usize,
}

/// The text blocks of `blocks` joined, and how many blocks are not text.
fn text_blocks(blocks: &[Value]) -> (String, usize) {
    let mut text = String::new();
    let mut others = 0;
    for block in blocks {
        match block.get("text").and_then(Value::as_str) {
            Some(part) if block.get("type").and_then(Value::as_str) == Some("text") => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(part);
            }
            _ => others += 1,
        }
    }
    (text, others)
}

/// What a `tool_result` block and, where it is this result's, the record's
/// `toolUseResult` hold. The block's `content` is what the model was given;
/// `toolUseResult` is Claude Code's own record of the call, whose shape
/// depends on the tool: an object with `stdout` / `stderr` / `interrupted`
/// for a command, a string for an error, an array of blocks for an MCP tool.
pub(crate) fn output_parts<'a>(
    block: &'a Map<String, Value>,
    tool_use_result: Option<&'a Value>,
) -> OutputParts<'a> {
    use std::borrow::Cow;
    let (mut text, mut other_blocks) = match block.get("content") {
        Some(Value::String(text)) => (Cow::Borrowed(text.as_str()), 0),
        Some(Value::Array(blocks)) => {
            let (text, others) = text_blocks(blocks);
            (Cow::Owned(text), others)
        }
        _ => (Cow::Borrowed(""), 0),
    };
    let (mut stderr, mut interrupted) = (None, false);
    match tool_use_result {
        Some(Value::Object(result)) => {
            stderr = result.get("stderr").and_then(Value::as_str);
            interrupted = result.get("interrupted").and_then(Value::as_bool) == Some(true);
            if text.is_empty() {
                if let Some(stdout) = result.get("stdout").and_then(Value::as_str) {
                    text = Cow::Borrowed(stdout);
                }
            }
        }
        Some(Value::String(result)) if text.is_empty() => text = Cow::Borrowed(result),
        Some(Value::Array(blocks)) if text.is_empty() && other_blocks == 0 => {
            let (joined, others) = text_blocks(blocks);
            text = Cow::Owned(joined);
            other_blocks = others;
        }
        _ => {}
    }
    OutputParts {
        text,
        stderr,
        interrupted,
        other_blocks,
    }
}

impl ToolResult {
    /// A result is a success unless it says otherwise.
    pub fn success(&self) -> bool {
        self.is_error != Some(true)
    }
}

/// An `assistant` record.
///
/// Claude Code writes one record per content block: a response with thinking,
/// text and two tool calls is four records sharing one [`message_id`] and one
/// identical [`usage`]. Count usage once per `message_id`.
///
/// [`message_id`]: Assistant::message_id
/// [`usage`]: Assistant::usage
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Assistant {
    /// `message.id`: the API response this block belongs to.
    pub message_id: Option<String>,
    pub request_id: Option<String>,
    /// The `id` of the [`ApiRequest`] record that produced this response.
    pub request_ref: Option<String>,
    /// The position of this record's block within the response.
    pub block_index: Option<u64>,
    /// `message.model`. `<synthetic>` for messages Claude Code made up itself.
    pub model: Option<String>,
    pub usage: Option<Usage>,
    pub stop_reason: Option<String>,
    /// Tool calls in this record: name and id only.
    pub tool_uses: Vec<ToolUse>,
    /// The record reports an API error instead of a response.
    pub is_api_error: bool,
}

impl Assistant {
    fn from_object(obj: &Map<String, Value>) -> Self {
        let message = obj.get("message").and_then(Value::as_object);
        let tool_uses = content_blocks(obj)
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
            .map(|b| {
                let input = b.get("input");
                let summary = input.and_then(summarize_input);
                ToolUse {
                    id: string(b, "id"),
                    name: string(b, "name"),
                    // Kept only where the summary is not all of it.
                    input: input
                        .and_then(whole_input)
                        .filter(|whole| Some(whole) != summary.as_ref()),
                    summary,
                }
            })
            .collect();
        Self {
            message_id: message.and_then(|m| string(m, "id")),
            request_id: string(obj, "requestId"),
            request_ref: string(obj, "requestRef"),
            block_index: obj.get("apiBlockIndex").and_then(Value::as_u64),
            model: message.and_then(|m| string(m, "model")),
            usage: message
                .and_then(|m| m.get("usage"))
                .and_then(Usage::from_value),
            stop_reason: message.and_then(|m| string(m, "stop_reason")),
            tool_uses,
            is_api_error: flag(obj, "isApiErrorMessage"),
        }
    }
}

/// A tool call: which tool, the id its result will carry, and a line saying
/// what it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolUse {
    pub id: Option<String>,
    pub name: Option<String>,
    /// One line of the call's input: the command, the file path, the search
    /// pattern. See [`summarize_input`].
    pub summary: Option<String>,
    /// The text the summary was cut from, where the summary is not all of
    /// it: every line of the command. See [`whole_input`].
    pub input: Option<String>,
}

/// The longest summary of a tool input, in characters.
pub const SUMMARY_CHARS: usize = 120;

/// The most of a tool input that is kept, in characters.
pub const INPUT_CHARS: usize = 4000;

/// The input fields a summary is taken from, in order of preference.
const SUMMARY_KEYS: &[&str] = &[
    "command",
    "file_path",
    "notebook_path",
    "pattern",
    "url",
    "query",
    "description",
];

/// One line saying what a tool call is about: the first of the input's
/// `command`, `file_path`, `notebook_path`, `pattern`, `url`, `query` or
/// `description` that holds text, cut to its first line and to
/// [`SUMMARY_CHARS`]. `None` for an input with none of them. The bridge mod
/// summarises a running call the same way.
pub fn summarize_input(input: &Value) -> Option<String> {
    let line = described(input)?
        .lines()
        .next()
        .unwrap_or_default()
        .trim_end();
    Some(cut(line, SUMMARY_CHARS))
}

/// The text [`summarize_input`] takes its line from, with every line, cut to
/// [`INPUT_CHARS`]: a view shows it where one line is not enough.
pub fn whole_input(input: &Value) -> Option<String> {
    Some(cut(described(input)?, INPUT_CHARS))
}

/// The first of the summary's fields that holds text.
fn described(input: &Value) -> Option<&str> {
    SUMMARY_KEYS
        .iter()
        .filter_map(|key| input.get(key).and_then(Value::as_str))
        .map(str::trim)
        .find(|text| !text.is_empty())
}

/// `text`, or its first `max - 1` characters and an ellipsis.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max - 1).collect();
    cut.push('…');
    cut
}

/// `message.usage` of one API response.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// All cache writes, whatever their lifetime.
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    /// The share of the cache writes with a 5 minute lifetime, when the
    /// breakdown is present.
    pub cache_creation_5m_input_tokens: Option<u64>,
    /// The share with a 1 hour lifetime (priced higher), when present.
    pub cache_creation_1h_input_tokens: Option<u64>,
    /// The response ran in fast mode (`speed: "fast"`), which is priced
    /// higher.
    pub fast: bool,
}

impl Usage {
    fn from_value(value: &Value) -> Option<Self> {
        let obj = value.as_object()?;
        let count = |key: &str| obj.get(key).and_then(Value::as_u64);
        let breakdown = obj.get("cache_creation").and_then(Value::as_object);
        let split = |key: &str| breakdown.and_then(|b| b.get(key)).and_then(Value::as_u64);
        Some(Self {
            input_tokens: count("input_tokens").unwrap_or(0),
            output_tokens: count("output_tokens").unwrap_or(0),
            cache_creation_input_tokens: count("cache_creation_input_tokens").unwrap_or(0),
            cache_read_input_tokens: count("cache_read_input_tokens").unwrap_or(0),
            cache_creation_5m_input_tokens: split("ephemeral_5m_input_tokens"),
            cache_creation_1h_input_tokens: split("ephemeral_1h_input_tokens"),
            fast: obj.get("speed").and_then(Value::as_str) == Some("fast"),
        })
    }

    /// The tokens the model was given for this response: what the context
    /// held when it was sent.
    pub fn context_tokens(&self) -> u64 {
        self.input_tokens + self.cache_creation_input_tokens + self.cache_read_input_tokens
    }
}

/// A `system` record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct System {
    /// `turn_duration`, `compact_boundary`, `stop_hook_summary`, …
    pub subtype: Option<String>,
    /// On `turn_duration`: how long the turn took.
    pub duration_ms: Option<u64>,
    /// On `compact_boundary`.
    pub compact: Option<Compact>,
}

impl System {
    fn from_object(obj: &Map<String, Value>) -> Self {
        let compact = obj
            .get("compactMetadata")
            .and_then(Value::as_object)
            .map(|m| Compact {
                trigger: string(m, "trigger"),
                pre_tokens: m.get("preTokens").and_then(Value::as_u64),
                post_tokens: m.get("postTokens").and_then(Value::as_u64),
            });
        Self {
            subtype: string(obj, "subtype"),
            duration_ms: obj.get("durationMs").and_then(Value::as_u64),
            compact,
        }
    }
}

/// A compaction: the context before and after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compact {
    /// `auto` or `manual`.
    pub trigger: Option<String>,
    pub pre_tokens: Option<u64>,
    pub post_tokens: Option<u64>,
}

/// A session title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Title {
    pub title: Option<String>,
    pub source: TitleSource,
}

/// Where a title came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleSource {
    /// `ai-title`: generated by Claude Code.
    Ai,
    /// `custom-title`: set by the user.
    Custom,
    /// `summary`: the record older versions wrote.
    Summary,
}

/// A `last-prompt` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastPrompt {
    /// The prompt as typed. Absent on records that only move the leaf.
    pub text: Option<String>,
    /// The record the conversation ended on when this was written.
    pub leaf_uuid: Option<String>,
}

/// A `cost-state` record. Claude Code writes one when it exits; the total is
/// cumulative over the whole session, resumes included, and covers subagents
/// and requests the transcript does not record. A process that is killed
/// writes none, so the total can lag far behind what the transcript shows.
#[derive(Debug, Clone, PartialEq)]
pub struct CostState {
    pub total_cost_usd: Option<f64>,
}

/// An `api-request` record: one call to the API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiRequest {
    /// What assistant records name in `requestRef`.
    pub id: Option<String>,
    /// Who asked: `repl_main_thread`, `agent:builtin:Explore`, `sdk`, …
    pub query_source: Option<String>,
}

/// A point in time, in milliseconds since the Unix epoch (UTC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);

impl Timestamp {
    /// From milliseconds since the Unix epoch.
    pub fn from_millis(millis: i64) -> Self {
        Self(millis)
    }

    /// Milliseconds since the Unix epoch.
    pub fn as_millis(self) -> i64 {
        self.0
    }

    /// Parses an RFC 3339 timestamp (`2026-11-01T09:00:00.000Z`, or with a
    /// numeric offset). Fractions beyond milliseconds are truncated.
    pub fn parse(text: &str) -> Option<Self> {
        let b = text.as_bytes();
        if !text.is_ascii()
            || b.len() < 20
            || b[4] != b'-'
            || b[7] != b'-'
            || !matches!(b[10], b'T' | b't' | b' ')
            || b[13] != b':'
            || b[16] != b':'
        {
            return None;
        }
        let number = |from: usize, to: usize| -> Option<i64> {
            let digits = text.get(from..to)?;
            if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            digits.parse().ok()
        };
        let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
        let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
        if !(1..=12).contains(&month)
            || !(1..=31).contains(&day)
            || hour > 23
            || minute > 59
            || second > 60
        {
            return None;
        }

        let mut at = 19;
        let mut millis = 0;
        if b[at] == b'.' {
            let start = at + 1;
            at = start;
            while at < b.len() && b[at].is_ascii_digit() {
                at += 1;
            }
            if at == start {
                return None;
            }
            for i in 0..3 {
                let digit = b[start..at].get(i).map_or(0, |c| i64::from(c - b'0'));
                millis = millis * 10 + digit;
            }
        }

        let offset_seconds = match &text[at..] {
            "Z" | "z" => 0,
            zone => {
                let z = zone.as_bytes();
                if z.len() != 6 || z[3] != b':' {
                    return None;
                }
                let sign = match z[0] {
                    b'+' => 1,
                    b'-' => -1,
                    _ => return None,
                };
                sign * (number(at + 1, at + 3)? * 3600 + number(at + 4, at + 6)? * 60)
            }
        };

        let seconds =
            days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
                - offset_seconds;
        Some(Self(seconds * 1000 + millis))
    }
}

/// Days from 1970-01-01 to the given proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn string(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn flag(obj: &Map<String, Value>, key: &str) -> bool {
    obj.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// The object blocks of `message.content`. A string content (a typed prompt)
/// has none.
fn content_blocks(obj: &Map<String, Value>) -> impl Iterator<Item = &Map<String, Value>> {
    obj.get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_epoch_and_known_instants() {
        assert_eq!(Timestamp::parse("1970-01-01T00:00:00Z"), Some(Timestamp(0)));
        assert_eq!(
            Timestamp::parse("2000-01-01T00:00:00.000Z"),
            Some(Timestamp(946_684_800_000))
        );
        assert_eq!(
            Timestamp::parse("2024-02-29T12:34:56.789Z"),
            Some(Timestamp(1_709_210_096_789))
        );
    }

    #[test]
    fn timestamp_offsets_and_fractions() {
        let utc = Timestamp::parse("2026-11-01T00:00:00.000Z").unwrap();
        assert_eq!(Timestamp::parse("2026-11-01T09:00:00+09:00"), Some(utc));
        assert_eq!(Timestamp::parse("2026-10-31T19:00:00-05:00"), Some(utc));
        assert_eq!(
            Timestamp::parse("2026-11-01T00:00:00.5Z").map(Timestamp::as_millis),
            Some(utc.as_millis() + 500)
        );
        assert_eq!(
            Timestamp::parse("2026-11-01T00:00:00.123456789Z").map(Timestamp::as_millis),
            Some(utc.as_millis() + 123)
        );
    }

    #[test]
    fn timestamp_rejects_what_it_cannot_read() {
        for text in [
            "",
            "2026-11-01",
            "2026-11-01T09:00:00",
            "2026-13-01T09:00:00Z",
            "2026-11-01T25:00:00Z",
            "2026-11-01T09:00:00.Z",
            "2026-11-01T09:00:00+0900",
            "２０２６-11-01T09:00:00Z",
            "not a timestamp at all",
        ] {
            assert_eq!(Timestamp::parse(text), None, "{text}");
        }
    }

    #[test]
    fn timestamps_order_by_instant() {
        let earlier = Timestamp::parse("2026-11-01T09:00:00.000Z").unwrap();
        let later = Timestamp::parse("2026-11-01T09:00:00.001Z").unwrap();
        assert!(earlier < later);
    }

    #[test]
    fn a_line_that_is_not_an_object_is_invalid() {
        for line in ["", "not json", "{\"type\":\"user\",", "42", "[]", "null"] {
            assert_eq!(Record::parse(line).view(), &View::Invalid, "{line}");
        }
        assert_eq!(Record::parse_bytes(&[0xff, 0xfe]).view(), &View::Invalid);
    }

    #[test]
    fn a_modelled_record_missing_its_keys_still_parses() {
        let record = Record::parse(r#"{"type":"assistant"}"#);
        assert_eq!(record.view(), &View::Assistant(Assistant::default()));
        let record = Record::parse(r#"{"type":"user","message":{"content":7}}"#);
        assert_eq!(record.view(), &View::User(User::default()));
    }

    #[test]
    fn a_tool_input_is_summarised_in_one_short_line() {
        let summary = |input: &str| summarize_input(&serde_json::from_str(input).unwrap());
        assert_eq!(
            summary(r#"{"command":"cargo test","description":"runs the tests"}"#).as_deref(),
            Some("cargo test")
        );
        assert_eq!(
            summary(r#"{"file_path":"/work/a.rs","old_string":"x","new_string":"y"}"#).as_deref(),
            Some("/work/a.rs")
        );
        assert_eq!(
            summary(r#"{"pattern":"fn main","path":"/work"}"#).as_deref(),
            Some("fn main")
        );
        assert_eq!(
            summary(r#"{"description":"Explore the repo","prompt":"long instructions"}"#)
                .as_deref(),
            Some("Explore the repo")
        );
        // The first line only, and an empty field is passed over.
        assert_eq!(
            summary(r#"{"command":"  ","url":"https://example.com/a\nsecond line"}"#).as_deref(),
            Some("https://example.com/a")
        );
        assert_eq!(
            summary(r#"{"command":"echo one\necho two"}"#).as_deref(),
            Some("echo one")
        );
        // Cut to the limit, counting characters, not bytes.
        let long = summary(&format!(r#"{{"command":"{}"}}"#, "あ".repeat(300))).unwrap();
        assert_eq!(long.chars().count(), SUMMARY_CHARS);
        assert!(long.ends_with('…'));
        // Nothing to summarise.
        assert_eq!(summary(r#"{"content":"a whole file"}"#), None);

        // A prompt: its first line with text; a command's name; nothing
        // for what was not typed.
        assert_eq!(
            summarize_prompt("\n  fix the build \nand the tests").as_deref(),
            Some("fix the build")
        );
        assert_eq!(
            summarize_prompt(
                "<command-message>review</command-message>\n<command-name>/review</command-name>"
            )
            .as_deref(),
            Some("/review")
        );
        assert_eq!(
            summarize_prompt("<system-reminder>injected</system-reminder>"),
            None
        );
        assert_eq!(summarize_prompt("<command-name></command-name>"), None);
        assert_eq!(summarize_prompt("  \n "), None);
        let long = summarize_prompt(&"あ".repeat(300)).unwrap();
        assert_eq!(long.chars().count(), PROMPT_CHARS);
        let prompt = |record: &str| match Record::parse(record).view() {
            View::User(user) => user.prompt.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            prompt(r#"{"type":"user","message":{"content":"run the tests\nplease"}}"#).as_deref(),
            Some("run the tests")
        );
        assert_eq!(
            prompt(
                r#"{"type":"user","message":{"content":[{"type":"image"},{"type":"text","text":"what is this"}]}}"#
            )
            .as_deref(),
            Some("what is this")
        );
        // Not a prompt: a tool's result, an injected record, a subagent's
        // instructions.
        for other in [
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t"},{"type":"text","text":"x"}]}}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"x"}}"#,
            r#"{"type":"user","isCompactSummary":true,"message":{"content":"x"}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":"x"}}"#,
            r#"{"type":"user","agentId":"a1","message":{"content":"x"}}"#,
            r#"{"type":"user","message":{"content":7}}"#,
            r#"{"type":"user"}"#,
        ] {
            assert_eq!(prompt(other), None, "{other}");
        }

        // The whole text is kept beside a summary that is not all of it.
        let whole = |input: &str| whole_input(&serde_json::from_str(input).unwrap());
        assert_eq!(
            whole(r#"{"command":"echo one\necho two\n"}"#).as_deref(),
            Some("echo one\necho two")
        );
        let long = whole(&format!(r#"{{"command":"{}"}}"#, "あ".repeat(5000))).unwrap();
        assert_eq!(long.chars().count(), INPUT_CHARS);
        assert_eq!(whole(r#"{"content":"a whole file"}"#), None);
        let uses = |input: &str| {
            let line = format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"t","name":"Bash","input":{input}}}]}}}}"#
            );
            match Record::parse(&line).view() {
                View::Assistant(assistant) => assistant.tool_uses[0].clone(),
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(uses(r#"{"command":"cargo test"}"#).input, None);
        assert_eq!(
            uses(r#"{"command":"echo one\necho two"}"#).input.as_deref(),
            Some("echo one\necho two")
        );
        assert_eq!(summary(r#"{"command":7}"#), None);
        assert_eq!(summary("[]"), None);
    }

    #[test]
    fn session_id_falls_back_to_the_snake_case_key() {
        let record = Record::parse(r#"{"type":"user","session_id":"s"}"#);
        assert_eq!(record.common().session_id.as_deref(), Some("s"));
    }
}
