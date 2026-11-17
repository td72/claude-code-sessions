//! Applying an [`Event`] to a [`SessionStore`].
//!
//! Live events write only [`Session::live`]: what the session is doing right
//! now, its context against the window in effect, its rate limits. Tokens,
//! cost and history belong to the transcripts and are never touched, so it
//! does not matter which of the two inputs arrives first.
//!
//! Events can arrive out of order (each is its own request). Within the live
//! side the latest timestamp wins: an event older than the state it would
//! change is dropped, with one exception — the end of a tool call always
//! removes that call, whenever it arrives, because a call never un-ends.
//!
//! An event for a session the transcripts have not shown yet creates the
//! session; its transcript joins it by id when it appears.
//!
//! [`Session::live`]: iktinos_core::session::Session::live

use std::path::PathBuf;

use iktinos_core::record::Timestamp;
use iktinos_core::session::{
    Context, Live, LiveTurn, PendingApproval, RateLimit, RunningTool, SessionStore,
};

use crate::event::{Body, Decision, Event, Phase, SessionMeasure, ToolCall, ToolCheck};

/// Applies one event to the session it names.
pub fn apply_event(store: &mut SessionStore, event: &Event) {
    if matches!(event.body, Body::Unknown) {
        return;
    }
    let at = event.timestamp();
    let live = store.live_mut(&event.session_id);
    match &event.body {
        Body::SessionMeasure(measure) => apply_measure(live, measure, at),
        Body::ToolCall(call) if call.phase == Phase::End => {
            // Applies whatever its age: see the module documentation.
            end_tool(live, call);
            advance(live, at);
        }
        body => {
            if live.state_at.is_some_and(|latest| at < latest) {
                return;
            }
            live.state_at = Some(at);
            apply_state(live, body, at);
        }
    }
}

fn apply_state(live: &mut Live, body: &Body, at: Timestamp) {
    match body {
        Body::SessionStart(start) => {
            // Also sent when the mod reloads in the middle of a session, so
            // it says nothing about the turn in progress.
            live.ended = false;
            live.cwd = Some(PathBuf::from(&start.cwd));
            if start.model.is_some() {
                live.model.clone_from(&start.model);
            }
        }
        Body::SessionEnd(_) => {
            live.ended = true;
            live.turn = None;
            live.running_tools.clear();
            live.awaiting_approval = None;
        }
        Body::TurnStart(turn) => {
            live.ended = false;
            if turn.agent_id.is_none() {
                live.turn = Some(LiveTurn {
                    id: turn.turn_id.clone(),
                    started_at: at,
                });
                live.idle_since = None;
            }
        }
        Body::TurnComplete(turn) => {
            // Whatever the thread had running or pending ended with its turn.
            let agent = turn.agent_id.as_deref();
            live.running_tools
                .retain(|tool| tool.agent_id.as_deref() != agent);
            if live
                .awaiting_approval
                .as_ref()
                .is_some_and(|p| p.agent_id.as_deref() == agent)
            {
                live.awaiting_approval = None;
            }
            if agent.is_none() {
                live.turn = None;
                live.idle_since = Some(at);
            }
        }
        Body::ToolCall(call) if call.phase == Phase::Start => {
            live.ended = false;
            clear_approval(live, call.tool_use_id.as_deref(), &call.tool);
            let already = call.tool_use_id.is_some()
                && live
                    .running_tools
                    .iter()
                    .any(|t| t.tool_use_id == call.tool_use_id);
            if !already {
                live.running_tools.push(RunningTool {
                    tool_use_id: call.tool_use_id.clone(),
                    name: call.tool.clone(),
                    summary: call.summary.clone(),
                    agent_id: call.agent_id.clone(),
                    started_at: at,
                });
            }
        }
        Body::ToolCheck(check) => apply_check(live, check, at),
        // A spawned agent shows up through its own turns and tool calls, and
        // in the transcripts; a phase this version does not know says nothing.
        _ => {}
    }
}

fn apply_check(live: &mut Live, check: &ToolCheck, at: Timestamp) {
    match check.decision {
        Decision::Ask => {
            live.awaiting_approval = Some(PendingApproval {
                tool_use_id: check.tool_use_id.clone(),
                name: check.tool.clone(),
                agent_id: check.agent_id.clone(),
                since: at,
            });
        }
        Decision::Allow | Decision::Deny => {
            clear_approval(live, check.tool_use_id.as_deref(), &check.tool);
        }
        Decision::Unknown => {}
    }
}

fn apply_measure(live: &mut Live, measure: &SessionMeasure, at: Timestamp) {
    if live.measured_at.is_some_and(|latest| at < latest) {
        return;
    }
    live.measured_at = Some(at);
    if let Some(context) = measure.context {
        // Before the first response there is a window and no count yet.
        live.context = context.tokens.map(|tokens| Context {
            tokens,
            window: Some(context.window),
        });
    }
    if !measure.rate_limits.is_empty() {
        live.rate_limits = measure
            .rate_limits
            .iter()
            .map(|limit| RateLimit {
                kind: limit.kind.clone(),
                percent_used: limit.percent_used,
                resets_at: limit.resets_at.clone(),
            })
            .collect();
    }
    // `cost_usd` is deliberately not applied: cost is the transcripts'.
}

fn end_tool(live: &mut Live, call: &ToolCall) {
    let index = match &call.tool_use_id {
        Some(id) => live
            .running_tools
            .iter()
            .position(|t| t.tool_use_id.as_ref() == Some(id)),
        // Without an id, the oldest running call of that tool by that thread.
        None => live
            .running_tools
            .iter()
            .position(|t| t.name == call.tool && t.agent_id == call.agent_id),
    };
    if let Some(index) = index {
        live.running_tools.remove(index);
    }
    clear_approval(live, call.tool_use_id.as_deref(), &call.tool);
}

/// A call that started, ended or was decided is no longer waiting.
fn clear_approval(live: &mut Live, tool_use_id: Option<&str>, tool: &str) {
    let resolved = live.awaiting_approval.as_ref().is_some_and(|pending| {
        match (pending.tool_use_id.as_deref(), tool_use_id) {
            (Some(pending_id), Some(id)) => pending_id == id,
            _ => pending.name == tool,
        }
    });
    if resolved {
        live.awaiting_approval = None;
    }
}

fn advance(live: &mut Live, at: Timestamp) {
    if live.state_at.is_none_or(|latest| at > latest) {
        live.state_at = Some(at);
    }
}
