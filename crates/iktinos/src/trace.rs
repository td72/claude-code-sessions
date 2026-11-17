//! A session's latest interactions as trees of spans: where the time went.
//!
//! The names and the hierarchy are those of the traces Claude Code exports
//! itself through OpenTelemetry (`claude_code.interaction`,
//! `claude_code.llm_request`, `claude_code.tool`), rebuilt here from the
//! transcript:
//!
//! ```text
//! interaction
//! ├── LLM request
//! ├── tool call
//! │   └── subagent: its LLM requests and tool calls
//! └── LLM request
//! ```
//!
//! - An [`Interaction`] starts at a prompt on the main thread and ends at
//!   Claude Code's `turn_duration` record. One without that record is still
//!   running, or was cut off: the transcript cannot tell which. (The
//!   transcript calls this a turn. The Agent SDK's `max_turns` counts
//!   something else, one pass of the agent loop, which here is an LLM
//!   request and the tool calls that follow it.)
//! - An [`LlmRequest`] span is one API response. It starts at its
//!   `api-request` record, or at the record before its first block where
//!   there is none, and ends at its last block. **Its duration is an
//!   approximation**: the transcript records when blocks were written, not
//!   when the API answered.
//! - A tool call's span runs from its `tool_use` block to its result. LLM
//!   requests and tool calls are siblings, in the order they happened.
//! - A subagent hangs under the tool call that spawned it (`toolUseId` in
//!   its metadata file). One without that link is placed by time, directly
//!   under the interaction it started in.
//!
//! What the exported traces have and the transcript does not: the time a
//! tool call waited for approval, hooks, and retries.
//!
//! The spans of the main thread live in the interaction, a subagent's in
//! the [`Subagent`]; each in a [`Thread`], in the order they were read. The
//! tree is put together when it is read ([`Session::walk`]), so the order
//! the session's files are read in does not change it.
//!
//! Everything is bounded: a session keeps its latest interactions
//! ([`DEFAULT_TRACE_INTERACTIONS`] unless the store is told otherwise), a
//! thread at most [`MAX_SPANS`] spans, and a subagent's spans go when the
//! interactions it was active in have gone.

use std::collections::{HashMap, HashSet};

use crate::pricing::Tokens;
use crate::record::Timestamp;
use crate::session::{Session, Subagent, ToolCall};

/// How many of its latest interactions a session keeps unless told
/// otherwise.
pub const DEFAULT_TRACE_INTERACTIONS: usize = 20;

/// The most spans one thread (an interaction's main thread, or a subagent)
/// keeps.
pub const MAX_SPANS: usize = 500;

/// One interaction: a prompt and everything done to answer it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Interaction {
    /// The prompt's `promptId`, else its record's `uuid`. Empty for an
    /// interaction whose prompt was not read (the transcript was read from
    /// the middle).
    pub id: String,
    pub started_at: Option<Timestamp>,
    /// When Claude Code wrote the interaction's duration. `None` for one
    /// that is running or was cut off.
    pub ended_at: Option<Timestamp>,
    /// The duration Claude Code measured itself.
    pub duration_ms: Option<u64>,
    /// The main thread's spans.
    pub thread: Thread,
}

impl Interaction {
    pub(crate) fn new(id: String, started_at: Option<Timestamp>) -> Self {
        Self {
            id,
            started_at,
            ended_at: None,
            duration_ms: None,
            thread: Thread::default(),
        }
    }
}

/// The spans of one thread in the order they were read.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct Thread {
    pub spans: Vec<Span>,
    /// Spans that were not kept because the thread was full.
    pub dropped: u64,
}

/// One span: an LLM request or a tool call.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Span {
    pub started_at: Option<Timestamp>,
    /// `None` while the span runs, and for one whose end was never read.
    pub ended_at: Option<Timestamp>,
    pub kind: SpanKind,
}

impl Span {
    /// From start to end. `None` unless both are known.
    pub fn duration_ms(&self) -> Option<u64> {
        between(self.started_at, self.ended_at)
    }
}

/// The milliseconds from `start` to `end`, when both are known.
pub fn between(start: Option<Timestamp>, end: Option<Timestamp>) -> Option<u64> {
    let (start, end) = (start?, end?);
    Some(end.as_millis().saturating_sub(start.as_millis()).max(0) as u64)
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SpanKind {
    LlmRequest(LlmRequest),
    Tool(ToolCall),
}

/// One API response.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct LlmRequest {
    /// `message.id`: several records are one response.
    pub message_id: Option<String>,
    pub model: Option<String>,
    /// Who asked: `repl_main_thread`, `agent:builtin:Explore`, …
    pub query_source: Option<String>,
    pub tokens: Option<Tokens>,
    /// The estimated cost. `None` for a model the price table does not know.
    pub usd: Option<f64>,
    /// `end_turn`, `tool_use`, … of the last block read.
    pub stop_reason: Option<String>,
}

impl Thread {
    /// A block of a response arrived at `at`: its request, made a span if
    /// this is the first block. `None` when the thread is full.
    pub(crate) fn respond(
        &mut self,
        message_id: Option<&str>,
        started_at: Option<Timestamp>,
        at: Option<Timestamp>,
    ) -> Option<&mut LlmRequest> {
        // The blocks of one response are written together: it is among the
        // last spans if it is there at all.
        let known = message_id.and_then(|id| {
            self.spans.iter().rposition(|span| {
                matches!(&span.kind, SpanKind::LlmRequest(r) if r.message_id.as_deref() == Some(id))
            })
        });
        let index = match known {
            Some(index) => index,
            None => {
                self.push(Span {
                    started_at,
                    ended_at: None,
                    kind: SpanKind::LlmRequest(LlmRequest {
                        message_id: message_id.map(str::to_owned),
                        ..LlmRequest::default()
                    }),
                })?;
                self.spans.len() - 1
            }
        };
        let span = &mut self.spans[index];
        span.ended_at = span.ended_at.max(at);
        match &mut span.kind {
            SpanKind::LlmRequest(request) => Some(request),
            SpanKind::Tool(_) => None,
        }
    }

    /// A tool call.
    pub(crate) fn call(&mut self, call: ToolCall) {
        self.push(Span {
            started_at: call.started_at,
            ended_at: None,
            kind: SpanKind::Tool(call),
        });
    }

    /// The result of a tool call arrived.
    pub(crate) fn result(&mut self, tool_use_id: &str, at: Option<Timestamp>, success: bool) {
        let found = self
            .spans
            .iter_mut()
            .rev()
            .find_map(|span| match &mut span.kind {
                SpanKind::Tool(call) if call.id == tool_use_id => {
                    Some((call, &mut span.ended_at, span.started_at))
                }
                _ => None,
            });
        if let Some((call, ended_at, started_at)) = found {
            *ended_at = at;
            call.success = Some(success);
            call.duration_ms = between(started_at, at);
        }
    }

    fn push(&mut self, span: Span) -> Option<()> {
        if self.spans.len() >= MAX_SPANS {
            self.dropped += 1;
            return None;
        }
        self.spans.push(span);
        Some(())
    }
}

/// One row of an interaction's tree.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Node<'a> {
    /// 0 for what hangs directly under the interaction.
    pub depth: usize,
    pub item: Item<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Item<'a> {
    Span(&'a Span),
    /// A subagent; its own spans follow it, one level deeper.
    Subagent(&'a Subagent),
}

impl Node<'_> {
    pub fn started_at(&self) -> Option<Timestamp> {
        match self.item {
            Item::Span(span) => span.started_at,
            Item::Subagent(subagent) => subagent.started_at,
        }
    }

    /// For a subagent, its latest record: whether it has finished is in
    /// [`Subagent::finished`].
    pub fn ended_at(&self) -> Option<Timestamp> {
        match self.item {
            Item::Span(span) => span.ended_at,
            Item::Subagent(subagent) => subagent.last_activity,
        }
    }

    pub fn duration_ms(&self) -> Option<u64> {
        between(self.started_at(), self.ended_at())
    }
}

impl Session {
    /// An interaction of this session as a tree, depth first: its LLM
    /// requests and tool calls in the order they happened, under a tool
    /// call the subagents it spawned and what they did. Then the subagents
    /// that started during the interaction and are not linked to a tool
    /// call that is kept.
    pub fn walk<'a>(&'a self, interaction: &'a Interaction) -> Vec<Node<'a>> {
        let mut spawned: HashMap<&str, Vec<&Subagent>> = HashMap::new();
        for subagent in self.subagents.values() {
            if let Some(id) = subagent.tool_use_id.as_deref() {
                spawned.entry(id).or_default().push(subagent);
            }
        }
        let mut walk = Walk {
            spawned,
            placed: HashSet::new(),
            nodes: Vec::new(),
        };
        walk.thread(&interaction.thread, 0);

        // The window the interaction covers: up to the next one's start.
        let next = self
            .trace
            .iter()
            .position(|other| std::ptr::eq(other, interaction))
            .and_then(|at| self.trace.get(at + 1))
            .and_then(|next| next.started_at);
        let within = |subagent: &Subagent| match (subagent.started_at, interaction.started_at) {
            (Some(at), Some(start)) => at >= start && next.is_none_or(|next| at < next),
            _ => false,
        };
        let strays: Vec<&Subagent> = self
            .subagents
            .values()
            .filter(|subagent| !walk.placed.contains(subagent.agent_id.as_str()))
            .filter(|subagent| within(subagent))
            .filter(|subagent| {
                subagent
                    .tool_use_id
                    .as_deref()
                    .is_none_or(|id| !self.has_tool_span(id))
            })
            .collect();
        for subagent in strays {
            walk.subagent(subagent, 0);
        }
        walk.nodes
    }

    /// Whether a tool call with this id is kept as a span anywhere.
    fn has_tool_span(&self, tool_use_id: &str) -> bool {
        let threads = self
            .trace
            .iter()
            .map(|interaction| &interaction.thread)
            .chain(self.subagents.values().map(|subagent| &subagent.thread));
        threads
            .flat_map(|thread| &thread.spans)
            .any(|span| matches!(&span.kind, SpanKind::Tool(call) if call.id == tool_use_id))
    }
}

struct Walk<'a> {
    /// Subagents by the tool call that spawned them.
    spawned: HashMap<&'a str, Vec<&'a Subagent>>,
    /// The subagents already in the tree: none appears twice.
    placed: HashSet<&'a str>,
    nodes: Vec<Node<'a>>,
}

impl<'a> Walk<'a> {
    fn thread(&mut self, thread: &'a Thread, depth: usize) {
        for span in &thread.spans {
            self.nodes.push(Node {
                depth,
                item: Item::Span(span),
            });
            if let SpanKind::Tool(call) = &span.kind {
                let spawned = self.spawned.get(call.id.as_str()).cloned();
                for subagent in spawned.unwrap_or_default() {
                    self.subagent(subagent, depth + 1);
                }
            }
        }
    }

    fn subagent(&mut self, subagent: &'a Subagent, depth: usize) {
        if !self.placed.insert(subagent.agent_id.as_str()) {
            return;
        }
        self.nodes.push(Node {
            depth,
            item: Item::Subagent(subagent),
        });
        self.thread(&subagent.thread, depth + 1);
    }
}
