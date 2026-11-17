//! The drift survey: what a corpus contains that the views do not know.
//!
//! The transcript format is undocumented and moves with every Claude Code
//! release. Feed every record of a corpus to a [`Drift`] and it counts the
//! record types, keys, content-block types, attachment types and system
//! subtypes missing from the tables below, plus the `version` values seen.
//! An empty survey after a Claude Code upgrade means the views still fit; a
//! non-empty one is the list of things to look at before bumping
//! [`KNOWN_CLAUDE_CODE_VERSION`].
//!
//! Only names and counts are collected, never values.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use serde_json::{Map, Value};

use super::{Record, View, KNOWN_CLAUDE_CODE_VERSION};

/// Keys carried by every conversational record.
const SHARED_KEYS: &[&str] = &[
    "agentId",
    "cwd",
    "entrypoint",
    "gitBranch",
    "isMeta",
    "isSidechain",
    "parentUuid",
    "sessionId",
    "session_id",
    "slug",
    "timestamp",
    "type",
    "userType",
    "uuid",
    "version",
];

const USER_KEYS: &[&str] = &[
    "imagePasteIds",
    "interruptedBySendNow",
    "interruptedMessageId",
    "isCompactSummary",
    "isVisibleInTranscriptOnly",
    "mcpMeta",
    "message",
    "origin",
    "permissionDecision",
    "permissionMode",
    "promptId",
    "promptSource",
    "queueSkipAttachments",
    "queueSkipSlashCommands",
    "queueTranscriptOnly",
    "sandboxed",
    "serverClassifierContext",
    "sourceToolAssistantUUID",
    "sourceToolUseID",
    "toolDenialKind",
    "toolEndsTurn",
    "toolUseResult",
    "turnCompanion",
    "turnOrigin",
    "turnPosition",
];

const ASSISTANT_KEYS: &[&str] = &[
    "advisorModel",
    "apiBlockIndex",
    "apiError",
    "apiErrorStatus",
    "attributionAgent",
    "attributionMcpServer",
    "attributionMcpTool",
    "attributionPlugin",
    "attributionSkill",
    "effort",
    "error",
    "errorDetails",
    "failedResponseHadNoToolCall",
    "isAbortedMidStream",
    "isApiErrorMessage",
    "madeBy",
    "message",
    "perTurnEffort",
    "promptCacheRequested",
    "requestId",
    "requestRef",
    "requestedModel",
    "serverClassifierRequest",
    "streamCutCause",
    "thinkingDisplay",
    "thinkingDurationMs",
    "toolInputStreamMs",
    "truncatedAfterOutput",
    "wireIngestContext",
    "wireToolInputs",
];

const SYSTEM_KEYS: &[&str] = &[
    "commandOutcome",
    "commandRun",
    "compactMetadata",
    "content",
    "durationMs",
    "hasOutput",
    "hookAdditionalContext",
    "hookCount",
    "hookErrors",
    "hookInfos",
    "level",
    "logicalParentUuid",
    "messageCount",
    "pendingBackgroundAgentCount",
    "preventedContinuation",
    "stopReason",
    "subtype",
    "toolUseID",
    "url",
];

const TITLE_KEYS: &[&str] = &[
    "aiTitle",
    "customTitle",
    "leafUuid",
    "sessionId",
    "summary",
    "type",
];

const LAST_PROMPT_KEYS: &[&str] = &["explicit", "lastPrompt", "leafUuid", "sessionId", "type"];

const API_REQUEST_KEYS: &[&str] = &[
    "agentId",
    "base",
    "cacheControl",
    "id",
    "keep",
    "params",
    "querySource",
    "requestDigest",
    "sessionId",
    "shapeHash",
    "tail",
    "timestamp",
    "type",
    "version",
];

const COST_STATE_KEYS: &[&str] = &[
    "hasUnknownModelCost",
    "modelUsage",
    "sessionId",
    "startTime",
    "totalAPIDuration",
    "totalAPIDurationWithoutRetries",
    "totalCostUSD",
    "totalDuration",
    "totalLinesAdded",
    "totalLinesRemoved",
    "totalToolDuration",
    "type",
];

const USER_MESSAGE_KEYS: &[&str] = &["content", "role"];

const ASSISTANT_MESSAGE_KEYS: &[&str] = &[
    "container",
    "content",
    "context_management",
    "diagnostics",
    "id",
    "input_transformations",
    "model",
    "role",
    "stop_details",
    "stop_reason",
    "stop_sequence",
    "type",
    "usage",
];

const USAGE_KEYS: &[&str] = &[
    "cache_creation",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
    "fallback_credit",
    "inference_geo",
    "input_tokens",
    "iterations",
    "output_tokens",
    "output_tokens_details",
    "server_tool_use",
    "service_tier",
    "speed",
];

const BLOCK_TYPES: &[&str] = &[
    "image",
    "text",
    "thinking",
    "tool_addition",
    "tool_result",
    "tool_use",
];

const SYSTEM_SUBTYPES: &[&str] = &[
    "away_summary",
    "bridge_status",
    "compact_boundary",
    "informational",
    "local_command",
    "stop_hook_summary",
    "turn_duration",
];

const ATTACHMENT_TYPES: &[&str] = &[
    "agent_listing_delta",
    "auto_mode",
    "bash_output_audience_note",
    "batching_reminder_sent",
    "command_permissions",
    "compact_file_reference",
    "credential_org",
    "date",
    "deferred_tools_delta",
    "deferred_tools_record",
    "diagnostics",
    "edited_text_file",
    "environment",
    "file",
    "hook_additional_context",
    "hook_cancelled",
    "hook_non_blocking_error",
    "hook_success",
    "instructions",
    "invoked_skills",
    "mcp_instructions_delta",
    "model",
    "prompt_snapshot",
    "queued_command",
    "read_truncation_notice",
    "remote_session_change",
    "session_context",
    "silent_turn_reminder",
    "skill_listing",
    "task_status",
    "thinking_drop",
    "total_tokens_reminder",
];

/// What a corpus contains that the views do not know. Names map to the number
/// of records they appeared in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Drift {
    /// Records observed, invalid lines included.
    pub records: u64,
    /// Lines that were not a JSON object.
    pub invalid: u64,
    /// `version` values seen. Bookkeeping records carry none.
    pub versions: BTreeMap<String, u64>,
    /// Record types never seen before. A record without a `type` counts
    /// under `<none>`.
    pub unknown_types: BTreeMap<String, u64>,
    /// Keys never seen on a modelled record, as a path:
    /// `assistant.newKey`, `assistant.message.usage.new_tokens`.
    pub unknown_keys: BTreeMap<String, u64>,
    /// Content-block types never seen, as `assistant.new_block`.
    pub unknown_block_types: BTreeMap<String, u64>,
    pub unknown_attachment_types: BTreeMap<String, u64>,
    pub unknown_system_subtypes: BTreeMap<String, u64>,
}

impl Drift {
    /// An empty survey.
    pub fn new() -> Self {
        Self::default()
    }

    /// Surveys a whole corpus.
    pub fn survey<'a>(records: impl IntoIterator<Item = &'a Record>) -> Self {
        let mut drift = Self::new();
        for record in records {
            drift.observe(record);
        }
        drift
    }

    /// Adds one record to the survey.
    pub fn observe(&mut self, record: &Record) {
        self.records += 1;
        let Some(obj) = record.raw().as_object() else {
            self.invalid += 1;
            return;
        };
        if let Some(version) = &record.common().version {
            bump(&mut self.versions, version);
        }
        let kind = record.kind().unwrap_or("<none>");
        match record.view() {
            View::User(_) => {
                self.keys(kind, obj, &[SHARED_KEYS, USER_KEYS]);
                self.message(kind, obj, USER_MESSAGE_KEYS);
                // `toolUseResult` is an object, a string or an array of
                // blocks, depending on the tool. Anything else is a shape
                // the output view does not read.
                let shape = match obj.get("toolUseResult") {
                    None | Some(Value::Object(_) | Value::String(_)) => None,
                    Some(Value::Array(blocks)) => blocks
                        .iter()
                        .any(|block| !block.is_object())
                        .then_some("array of non-objects"),
                    Some(Value::Null) => Some("null"),
                    Some(Value::Bool(_)) => Some("boolean"),
                    Some(Value::Number(_)) => Some("number"),
                };
                if let Some(shape) = shape {
                    bump(
                        &mut self.unknown_keys,
                        &format!("{kind}.toolUseResult: {shape}"),
                    );
                }
            }
            View::Assistant(_) => {
                self.keys(kind, obj, &[SHARED_KEYS, ASSISTANT_KEYS]);
                self.message(kind, obj, ASSISTANT_MESSAGE_KEYS);
            }
            View::System(_) => {
                self.keys(kind, obj, &[SHARED_KEYS, SYSTEM_KEYS]);
                if let Some(subtype) = obj.get("subtype").and_then(Value::as_str) {
                    if !SYSTEM_SUBTYPES.contains(&subtype) {
                        bump(&mut self.unknown_system_subtypes, subtype);
                    }
                }
            }
            View::Title(_) => self.keys(kind, obj, &[TITLE_KEYS]),
            View::LastPrompt(_) => self.keys(kind, obj, &[LAST_PROMPT_KEYS]),
            View::ApiRequest(_) => self.keys(kind, obj, &[API_REQUEST_KEYS]),
            View::CostState(_) => self.keys(kind, obj, &[COST_STATE_KEYS]),
            View::Other => {
                if kind == "attachment" {
                    let attachment = obj
                        .get("attachment")
                        .and_then(|a| a.get("type"))
                        .and_then(Value::as_str);
                    if let Some(name) = attachment {
                        if !ATTACHMENT_TYPES.contains(&name) {
                            bump(&mut self.unknown_attachment_types, name);
                        }
                    }
                }
            }
            View::Unknown => bump(&mut self.unknown_types, kind),
            View::Invalid => {}
        }
    }

    /// Folds another survey into this one.
    pub fn merge(&mut self, other: Drift) {
        self.records += other.records;
        self.invalid += other.invalid;
        for (ours, theirs) in [
            (&mut self.versions, other.versions),
            (&mut self.unknown_types, other.unknown_types),
            (&mut self.unknown_keys, other.unknown_keys),
            (&mut self.unknown_block_types, other.unknown_block_types),
            (
                &mut self.unknown_attachment_types,
                other.unknown_attachment_types,
            ),
            (
                &mut self.unknown_system_subtypes,
                other.unknown_system_subtypes,
            ),
        ] {
            for (name, count) in theirs {
                *ours.entry(name).or_default() += count;
            }
        }
    }

    /// Nothing unknown was seen. Invalid lines and newer versions do not
    /// count: a half-written last line is normal, and a newer version that
    /// introduces no new shape is fine.
    pub fn is_clean(&self) -> bool {
        self.unknown_types.is_empty()
            && self.unknown_keys.is_empty()
            && self.unknown_block_types.is_empty()
            && self.unknown_attachment_types.is_empty()
            && self.unknown_system_subtypes.is_empty()
    }

    /// The versions seen that are newer than [`KNOWN_CLAUDE_CODE_VERSION`].
    pub fn unchecked_versions(&self) -> Vec<&str> {
        self.versions
            .keys()
            .map(String::as_str)
            .filter(|v| compare_versions(v, KNOWN_CLAUDE_CODE_VERSION) == Ordering::Greater)
            .collect()
    }

    fn keys(&mut self, path: &str, obj: &Map<String, Value>, known: &[&[&str]]) {
        for key in obj.keys() {
            if !known.iter().any(|set| set.contains(&key.as_str())) {
                bump(&mut self.unknown_keys, &format!("{path}.{key}"));
            }
        }
    }

    fn message(&mut self, kind: &str, obj: &Map<String, Value>, known: &[&str]) {
        let Some(message) = obj.get("message").and_then(Value::as_object) else {
            return;
        };
        self.keys(&format!("{kind}.message"), message, &[known]);
        if let Some(usage) = message.get("usage").and_then(Value::as_object) {
            self.keys(&format!("{kind}.message.usage"), usage, &[USAGE_KEYS]);
        }
        let blocks = message.get("content").and_then(Value::as_array);
        for block in blocks.into_iter().flatten() {
            if let Some(name) = block.get("type").and_then(Value::as_str) {
                if !BLOCK_TYPES.contains(&name) {
                    bump(&mut self.unknown_block_types, &format!("{kind}.{name}"));
                }
            }
        }
    }
}

/// A plain-text report: counts and names only.
impl fmt::Display for Drift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "records: {} ({} invalid lines)",
            self.records, self.invalid
        )?;
        writeln!(f, "views checked against: {KNOWN_CLAUDE_CODE_VERSION}")?;
        let mut versions: Vec<_> = self.versions.iter().collect();
        versions.sort_by(|a, b| compare_versions(a.0, b.0));
        section(f, "versions seen", versions)?;
        let unchecked = self.unchecked_versions();
        if !unchecked.is_empty() {
            writeln!(f, "newer than checked: {}", unchecked.join(", "))?;
        }
        if self.is_clean() {
            return writeln!(f, "no unknown shapes");
        }
        section(f, "unknown record types", &self.unknown_types)?;
        section(f, "unknown keys", &self.unknown_keys)?;
        section(f, "unknown content-block types", &self.unknown_block_types)?;
        section(
            f,
            "unknown attachment types",
            &self.unknown_attachment_types,
        )?;
        section(f, "unknown system subtypes", &self.unknown_system_subtypes)
    }
}

fn section<'a>(
    f: &mut fmt::Formatter<'_>,
    title: &str,
    entries: impl IntoIterator<Item = (&'a String, &'a u64)>,
) -> fmt::Result {
    let mut entries = entries.into_iter().peekable();
    if entries.peek().is_none() {
        return Ok(());
    }
    writeln!(f, "{title}:")?;
    for (name, count) in entries {
        writeln!(f, "  {name} ×{count}")?;
    }
    Ok(())
}

fn bump(map: &mut BTreeMap<String, u64>, name: &str) {
    *map.entry(name.to_owned()).or_default() += 1;
}

/// Orders dotted numeric versions (`2.1.99` < `2.1.325`). A part that is not
/// a number sorts before every number.
fn compare_versions(a: &str, b: &str) -> Ordering {
    let parts = |v: &str| -> Vec<Option<u64>> { v.split('.').map(|p| p.parse().ok()).collect() };
    parts(a).cmp(&parts(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(compare_versions("2.1.99", "2.1.325"), Ordering::Less);
        assert_eq!(compare_versions("2.2.0", "2.1.325"), Ordering::Greater);
        assert_eq!(compare_versions("2.1.325", "2.1.325"), Ordering::Equal);
        assert_eq!(compare_versions("2.1", "2.1.0"), Ordering::Less);
    }

    #[test]
    fn a_newer_version_is_reported_but_does_not_dirty_the_survey() {
        let record = Record::parse(r#"{"type":"user","version":"99.0.0","message":{}}"#);
        let drift = Drift::survey([&record]);
        assert!(drift.is_clean());
        assert_eq!(drift.unchecked_versions(), ["99.0.0"]);
    }

    #[test]
    fn merge_adds_counts() {
        let unknown = Record::parse(r#"{"type":"never-seen"}"#);
        let mut total = Drift::survey([&unknown]);
        total.merge(Drift::survey([&unknown, &Record::parse("garbage")]));
        assert_eq!(total.records, 3);
        assert_eq!(total.invalid, 1);
        assert_eq!(total.unknown_types["never-seen"], 2);
    }

    #[test]
    fn a_record_without_a_type_is_counted() {
        let drift = Drift::survey([&Record::parse(r#"{"sessionId":"s"}"#)]);
        assert_eq!(drift.unknown_types["<none>"], 1);
    }
}
