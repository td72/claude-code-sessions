//! What the server returns: the model of the sessions, in the shapes the
//! page is drawn from.
//!
//! These are serve's own. They hold what the core models (a title, the first
//! line of a prompt, a command, the start of an output) and nothing of a raw
//! record. Times are milliseconds since the Unix epoch.

use std::time::Duration;

use iktinos_core::output::{WholeOutput, WholeResponse};
use iktinos_core::record::{Timestamp, ToolOutput};
use iktinos_core::session::{CostSource, ModelUsage, Session, StatusKind, Subagent, ToolCall};
use iktinos_core::trace::{between, Interaction, Item, LlmRequest, SpanKind};
use serde::Serialize;

fn ms(at: Option<Timestamp>) -> Option<i64> {
    at.map(Timestamp::as_millis)
}

#[derive(Debug, Serialize)]
pub struct Status {
    /// `busy`, `needs_approval`, `waiting`, `idle` or `exited`.
    pub kind: &'static str,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct Cost {
    pub usd: Option<f64>,
    /// `unknown`, `reported`, `reported_plus_estimate` or `estimated`.
    pub source: &'static str,
    /// Some of the session could not be priced.
    pub incomplete: bool,
}

/// One row of the list of sessions.
#[derive(Debug, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub label: String,
    pub location: Option<String>,
    pub location_name: Option<String>,
    pub branch: Option<String>,
    pub model: Option<String>,
    pub status: Status,
    pub started_at: Option<i64>,
    pub last_seen: Option<i64>,
    pub tokens: u64,
    pub cost: Cost,
    pub interactions: usize,
    pub tool_calls: u64,
    pub subagents: usize,
}

#[derive(Debug, Serialize)]
pub struct Context {
    pub tokens: u64,
    pub window: Option<u64>,
    /// 0 to 1, where the window is known.
    pub fill: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct Usage {
    pub name: String,
    pub responses: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
    /// `None` when nothing here could be priced.
    pub estimated_usd: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct RateLimit {
    pub kind: String,
    pub percent_used: f64,
    pub resets_at: Option<String>,
}

/// The start of what a tool call returned.
#[derive(Debug, Serialize)]
pub struct Output {
    pub text: String,
    pub stderr: Option<String>,
    /// There is more than `text` holds.
    pub truncated: bool,
    pub chars: usize,
    pub lines: usize,
    pub interrupted: bool,
    pub other_blocks: usize,
}

#[derive(Debug, Serialize)]
pub struct Call {
    pub id: String,
    pub name: String,
    pub summary: Option<String>,
    /// Every line of the command, where the summary is not all of it.
    pub input: Option<String>,
    pub agent_id: Option<String>,
    pub started_at: Option<i64>,
    pub duration_ms: Option<u64>,
    pub success: Option<bool>,
    pub output: Option<Output>,
}

#[derive(Debug, Serialize)]
pub struct SubagentSummary {
    pub agent_id: String,
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub model: Option<String>,
    pub finished: Option<bool>,
    pub started_at: Option<i64>,
    pub last_activity: Option<i64>,
    pub tokens: u64,
    pub estimated_usd: Option<f64>,
    pub tool_calls: u64,
}

/// One session, with what its detail shows.
#[derive(Debug, Serialize)]
pub struct SessionDetail {
    #[serde(flatten)]
    pub summary: SessionSummary,
    /// What the running tool is doing, where live events say.
    pub running: Option<String>,
    pub context: Option<Context>,
    pub by_model: Vec<Usage>,
    pub by_source: Vec<Usage>,
    pub rate_limits: Vec<RateLimit>,
    /// The latest first.
    pub recent_tools: Vec<Call>,
    pub subagents: Vec<SubagentSummary>,
}

/// One row of an interaction's tree.
#[derive(Debug, Serialize)]
pub struct Node {
    /// 0 for what hangs directly under the interaction.
    pub depth: usize,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub duration_ms: Option<u64>,
    #[serde(flatten)]
    pub what: What,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum What {
    LlmRequest {
        message_id: Option<String>,
        model: Option<String>,
        query_source: Option<String>,
        stop_reason: Option<String>,
        /// The first line of what the response said.
        text: Option<String>,
        thinking_ms: Option<u64>,
        rebuilt: bool,
        reshaped: bool,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        cache_read_tokens: Option<u64>,
        cache_write_tokens: Option<u64>,
        usd: Option<f64>,
    },
    Tool {
        #[serde(flatten)]
        call: Call,
    },
    Subagent {
        #[serde(flatten)]
        subagent: SubagentSummary,
    },
}

#[derive(Debug, Serialize)]
pub struct InteractionTree {
    pub id: String,
    pub prompt: Option<String>,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub duration_ms: Option<u64>,
    /// It has no end, it is the session's last, and the session is busy.
    pub running: bool,
    /// Spans that were not kept because the main thread was full.
    pub dropped: u64,
    pub nodes: Vec<Node>,
}

#[derive(Debug, Serialize)]
pub struct Trace {
    pub session: SessionSummary,
    /// The latest first.
    pub interactions: Vec<InteractionTree>,
}

/// Everything one tool call returned, read again from the transcript.
#[derive(Debug, Serialize)]
pub struct Whole {
    pub text: String,
    pub stderr: Option<String>,
    pub interrupted: bool,
    pub other_blocks: usize,
    /// The text was too long to read whole and is cut.
    pub truncated: bool,
}

fn status_kind(kind: StatusKind) -> &'static str {
    match kind {
        StatusKind::Busy => "busy",
        StatusKind::NeedsApproval => "needs_approval",
        StatusKind::Waiting => "waiting",
        StatusKind::Idle => "idle",
        StatusKind::Exited => "exited",
    }
}

pub fn summary(session: &Session, now: Timestamp, active_within: Duration) -> SessionSummary {
    let status = session.status(now, active_within);
    SessionSummary {
        id: session.id.clone(),
        label: session
            .label()
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned(),
        location: session
            .location()
            .map(|path| path.to_string_lossy().into_owned()),
        location_name: session.location_name(),
        branch: session.git_branch.clone(),
        model: session.model.clone(),
        status: Status {
            kind: status_kind(status.kind),
            text: status.text,
        },
        started_at: ms(session.started_at),
        last_seen: ms(session.last_seen()),
        tokens: session.usage.total().total_tokens(),
        cost: Cost {
            usd: session.cost.usd,
            source: match session.cost.source {
                CostSource::Unknown => "unknown",
                CostSource::Reported => "reported",
                CostSource::ReportedPlusEstimate => "reported_plus_estimate",
                CostSource::Estimated => "estimated",
            },
            incomplete: session.cost.incomplete,
        },
        interactions: session.trace.len(),
        tool_calls: session.tool_calls,
        subagents: session.subagents.len(),
    }
}

fn usage(name: &str, usage: &ModelUsage) -> Usage {
    Usage {
        name: name.to_owned(),
        responses: usage.responses,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_write_tokens: usage.cache_write_5m_tokens + usage.cache_write_1h_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        estimated_usd: usage.estimated_cost(),
    }
}

fn output(output: &ToolOutput) -> Output {
    Output {
        text: output.text.clone(),
        stderr: output.stderr.clone(),
        truncated: output.truncated,
        chars: output.chars,
        lines: output.lines,
        interrupted: output.interrupted,
        other_blocks: output.other_blocks,
    }
}

fn call(call: &ToolCall) -> Call {
    Call {
        id: call.id.clone(),
        name: call.name.clone(),
        summary: call.summary.clone(),
        input: call.input.clone(),
        agent_id: call.agent_id.clone(),
        started_at: ms(call.started_at),
        duration_ms: call.duration_ms,
        success: call.success,
        output: call.output.as_deref().map(output),
    }
}

fn subagent(subagent: &Subagent) -> SubagentSummary {
    SubagentSummary {
        agent_id: subagent.agent_id.clone(),
        agent_type: subagent.agent_type.clone(),
        description: subagent.description.clone(),
        model: subagent.model.clone(),
        finished: subagent.finished,
        started_at: ms(subagent.started_at),
        last_activity: ms(subagent.last_activity),
        tokens: subagent.usage.total_tokens(),
        estimated_usd: subagent.usage.estimated_cost(),
        tool_calls: subagent.tool_calls,
    }
}

pub fn detail(session: &Session, now: Timestamp, active_within: Duration) -> SessionDetail {
    let running = session.live.as_ref().and_then(|live| match live.state() {
        iktinos_core::session::LiveState::RunningTool(tool) => tool.summary.clone(),
        _ => None,
    });
    let mut recent_tools: Vec<Call> = session.recent_tools.iter().map(call).collect();
    recent_tools.reverse();
    SessionDetail {
        summary: summary(session, now, active_within),
        running,
        context: session.current_context().map(|context| Context {
            tokens: context.tokens,
            window: context.window,
            fill: context.fill(),
        }),
        by_model: session
            .usage
            .by_model
            .iter()
            .map(|(name, model)| usage(name, model))
            .collect(),
        by_source: session
            .usage
            .by_query_source
            .iter()
            .map(|(name, source)| usage(name, source))
            .collect(),
        rate_limits: session
            .live
            .iter()
            .flat_map(|live| &live.rate_limits)
            .map(|limit| RateLimit {
                kind: limit.kind.clone(),
                percent_used: limit.percent_used,
                resets_at: limit.resets_at.clone(),
            })
            .collect(),
        recent_tools,
        subagents: session.subagents.values().map(subagent).collect(),
    }
}

fn request(request: &LlmRequest) -> What {
    let tokens = request.tokens;
    What::LlmRequest {
        message_id: request.message_id.clone(),
        model: request.model.clone(),
        query_source: request.query_source.clone(),
        stop_reason: request.stop_reason.clone(),
        text: request.text.clone(),
        thinking_ms: request.thinking_ms,
        rebuilt: request.rebuilt,
        reshaped: request.reshaped,
        input_tokens: tokens.map(|t| t.input),
        output_tokens: tokens.map(|t| t.output),
        cache_read_tokens: tokens.map(|t| t.cache_read),
        cache_write_tokens: tokens.map(|t| t.cache_write_5m + t.cache_write_1h),
        usd: request.usd,
    }
}

fn tree(session: &Session, interaction: &Interaction, running: bool) -> InteractionTree {
    let nodes = session
        .walk(interaction)
        .iter()
        .map(|node| Node {
            depth: node.depth,
            started_at: ms(node.started_at()),
            ended_at: ms(node.ended_at()),
            duration_ms: node.duration_ms(),
            what: match node.item {
                Item::Span(span) => match &span.kind {
                    SpanKind::LlmRequest(llm) => request(llm),
                    SpanKind::Tool(tool) => What::Tool { call: call(tool) },
                    // A kind of span this server does not know yet.
                    _ => What::Tool {
                        call: Call {
                            id: String::new(),
                            name: "?".to_owned(),
                            summary: None,
                            input: None,
                            agent_id: None,
                            started_at: None,
                            duration_ms: None,
                            success: None,
                            output: None,
                        },
                    },
                },
                Item::Subagent(agent) => What::Subagent {
                    subagent: subagent(agent),
                },
            },
        })
        .collect();
    InteractionTree {
        id: interaction.id.clone(),
        prompt: interaction.prompt.clone(),
        started_at: ms(interaction.started_at),
        ended_at: ms(interaction.ended_at),
        duration_ms: interaction
            .duration_ms
            .or_else(|| between(interaction.started_at, interaction.ended_at)),
        running,
        dropped: interaction.thread.dropped,
        nodes,
    }
}

pub fn trace(session: &Session, now: Timestamp, active_within: Duration) -> Trace {
    let busy = session.status(now, active_within).kind == StatusKind::Busy;
    let last = session.trace.len().saturating_sub(1);
    let mut interactions: Vec<InteractionTree> = session
        .trace
        .iter()
        .enumerate()
        .map(|(index, interaction)| {
            let running = busy && index == last && interaction.ended_at.is_none();
            tree(session, interaction, running)
        })
        .collect();
    interactions.reverse();
    Trace {
        session: summary(session, now, active_within),
        interactions,
    }
}

pub fn whole_output(output: WholeOutput) -> Whole {
    Whole {
        text: output.text,
        stderr: output.stderr,
        interrupted: output.interrupted,
        other_blocks: output.other_blocks,
        truncated: output.truncated,
    }
}

pub fn whole_response(response: WholeResponse) -> Whole {
    Whole {
        text: response.text,
        stderr: None,
        interrupted: false,
        other_blocks: 0,
        truncated: response.truncated,
    }
}
