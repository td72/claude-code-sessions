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
    "cost-state",
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

impl User {
    fn from_object(obj: &Map<String, Value>) -> Self {
        let tool_results = content_blocks(obj)
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            .map(|b| ToolResult {
                tool_use_id: string(b, "tool_use_id"),
                is_error: b.get("is_error").and_then(Value::as_bool),
            })
            .collect();
        Self {
            tool_results,
            is_meta: flag(obj, "isMeta"),
            is_compact_summary: flag(obj, "isCompactSummary"),
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
            .map(|b| ToolUse {
                id: string(b, "id"),
                name: string(b, "name"),
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

/// A tool call: which tool, and the id its result will carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolUse {
    pub id: Option<String>,
    pub name: Option<String>,
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
    fn session_id_falls_back_to_the_snake_case_key() {
        let record = Record::parse(r#"{"type":"user","session_id":"s"}"#);
        assert_eq!(record.common().session_id.as_deref(), Some("s"));
    }
}
