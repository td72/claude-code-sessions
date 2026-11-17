//! The protocol the bridge mod speaks: one JSON object per event.
//!
//! ```json
//! {"v":1,"type":"tool.call","session_id":"…","ts":1793613602000,
//!  "phase":"start","tool":"Bash","tool_use_id":"toolu_01"}
//! ```
//!
//! Every event carries the protocol version `v`, the `session_id` it belongs
//! to and `ts`, milliseconds since the Unix epoch on the sending machine. The
//! rest depends on `type`, which mirrors the Claude Code mod event the bridge
//! observed.
//!
//! The protocol only grows. A consumer must tolerate what it does not know:
//! an unknown `type` parses to [`Body::Unknown`], unknown fields are dropped,
//! and an unknown value of an enumerated field parses to that enum's
//! `Unknown`. Only a different `v` is refused.
//!
//! Events stay small: names, ids, numbers and short summaries. No tool output
//! and no environment.
//!
//! The JSON files under `tests/fixtures/events/` are the contract the mod is
//! tested against.

use iktinos_core::record::Timestamp;
use serde::{Deserialize, Serialize};

/// The protocol version this crate reads and writes.
pub const VERSION: u32 = 1;

/// One event from the bridge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// The protocol version, always [`VERSION`] after [`Event::parse`].
    pub v: u32,
    pub session_id: String,
    /// Milliseconds since the Unix epoch.
    pub ts: i64,
    #[serde(flatten)]
    pub body: Body,
}

impl Event {
    /// Parses one event. Fails on anything that is not an event object of
    /// this protocol version; an unknown `type` is not a failure.
    pub fn parse(json: &[u8]) -> Result<Self, EventError> {
        let event: Self = serde_json::from_slice(json)?;
        if event.v != VERSION {
            return Err(EventError::UnsupportedVersion(event.v));
        }
        Ok(event)
    }

    /// When the event happened.
    pub fn timestamp(&self) -> Timestamp {
        Timestamp::from_millis(self.ts)
    }
}

/// What happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum Body {
    #[serde(rename = "session.start")]
    SessionStart(SessionStart),
    #[serde(rename = "session.end")]
    SessionEnd(SessionEnd),
    #[serde(rename = "turn.start")]
    TurnStart(TurnStart),
    #[serde(rename = "turn.complete")]
    TurnComplete(TurnComplete),
    /// The start or the end of a tool call.
    #[serde(rename = "tool.call")]
    ToolCall(ToolCall),
    /// The verdict of the permission check that precedes a tool call.
    #[serde(rename = "tool.check")]
    ToolCheck(ToolCheck),
    #[serde(rename = "session.measure")]
    SessionMeasure(SessionMeasure),
    #[serde(rename = "agent.spawn")]
    AgentSpawn(AgentSpawn),
    /// A `type` this version does not know. Ignored by the merge.
    #[serde(other, rename = "unknown")]
    Unknown,
}

/// A session started, or was resumed, or the mod was reloaded in it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStart {
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Someone is at a prompt, as opposed to `claude -p` or an SDK host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interactive: Option<bool>,
}

/// The process running the session is exiting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEnd {
    /// Why, as Claude Code names it (`clear`, `logout`, `prompt_input_exit`,
    /// …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A turn began: the session (or one of its subagents) is working.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnStart {
    pub turn_id: String,
    /// Set when the turn is a subagent's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

/// A turn ended: a main-thread turn ending means the session waits for its
/// user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnComplete {
    pub turn_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub reason: TurnEnd,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// How a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEnd {
    Answer,
    Aborted,
    Refusal,
    Error,
    #[serde(other)]
    Unknown,
}

/// The start or the end of one tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub phase: Phase,
    /// The tool's name (`Bash`, `mcp__server__tool`).
    pub tool: String,
    /// Pairs a start with its end, and with the transcript's `tool_use`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// Set when a subagent made the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// On a start: a short description of the call — the command, the file
    /// path — cut to a line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// On an end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// On an end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
}

/// Which side of a tool call an event reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Start,
    End,
    #[serde(other)]
    Unknown,
}

/// The verdict of a permission check. `ask` means the session is now waiting
/// for someone to approve the call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCheck {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub decision: Decision,
}

/// What a permission check decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Ask,
    Deny,
    #[serde(other)]
    Unknown,
}

/// Claude Code's own measurements, sent when one of them changes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionMeasure {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextUsage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rate_limits: Vec<RateLimit>,
    /// Claude Code's running total for the session. Informational: the
    /// session model takes cost from the transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// How full the context is, as Claude Code measures it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUsage {
    /// Absent before the first response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// The window in effect for this session, which may be smaller than the
    /// model's largest.
    pub window: u64,
}

/// One rate limit and how much of it is used.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimit {
    /// Which limit (`five_hour`, `seven_day`, …), as Claude Code names it.
    pub kind: String,
    /// 0–100.
    pub percent_used: f64,
    /// When the limit resets, as an RFC 3339 timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
}

/// A subagent was started.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSpawn {
    /// The parent's `tool_use` that spawned it.
    pub tool_use_id: String,
    /// Absent when Claude Code has not assigned one yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// `Explore`, `general-purpose`, …
    pub subagent_type: String,
    /// The short task description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
}

/// Bytes that are not an event of this protocol.
#[derive(Debug, thiserror::Error)]
pub enum EventError {
    #[error("not an event: {0}")]
    Json(#[from] serde_json::Error),
    #[error("protocol version {0} is not supported (this crate speaks version {VERSION})")]
    UnsupportedVersion(u32),
}
