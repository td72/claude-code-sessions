//! A session's latest turns as trees of spans: where a turn's time went.
//!
//! The hierarchy is turn → API request → tool call → subagent, and under a
//! subagent its own requests and tool calls:
//!
//! - A [`Turn`] starts at a prompt on the main thread and ends at Claude
//!   Code's `turn_duration` record. A turn without one is still running, or
//!   was cut off: the transcript cannot tell which.
//! - A [`Request`] span is one API response. It starts at its `api-request`
//!   record, or at the record before its first block where there is none,
//!   and ends at its last block. **Its duration is an approximation**: the
//!   transcript records when blocks were written, not when the API answered.
//! - A tool call's span runs from its `tool_use` block to its result.
//! - A subagent hangs under the tool call that spawned it (`toolUseId` in
//!   its metadata file). One without that link is placed by time, directly
//!   under the turn it started in.
//!
//! The spans of the main thread live in the turn, a subagent's in the
//! [`Subagent`]; each in a [`Thread`], in the order they were read, with the
//! index of their parent. The tree is put together when it is read
//! ([`Session::walk`]), so the order the session's files are read in does not
//! change it.
//!
//! Everything is bounded: a session keeps its latest turns
//! ([`DEFAULT_TRACE_TURNS`] unless the store is told otherwise), a thread at
//! most [`MAX_SPANS`] spans, and a subagent's spans go when the turns it was
//! active in have gone.

use std::collections::{HashMap, HashSet};

use crate::pricing::Tokens;
use crate::record::Timestamp;
use crate::session::{Session, Subagent, ToolCall};

/// How many of its latest turns a session keeps unless told otherwise.
pub const DEFAULT_TRACE_TURNS: usize = 20;

/// The most spans one thread (a turn's main thread, or a subagent) keeps.
pub const MAX_SPANS: usize = 500;

/// One turn of the main thread: a prompt and everything done to answer it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Turn {
    /// The prompt's `promptId`, else its record's `uuid`. Empty for a turn
    /// whose prompt was not read (the transcript was read from the middle).
    pub id: String,
    pub started_at: Option<Timestamp>,
    /// When Claude Code wrote the turn's duration. `None` for a turn that
    /// is running or was cut off.
    pub ended_at: Option<Timestamp>,
    /// The duration Claude Code measured itself.
    pub duration_ms: Option<u64>,
    /// The main thread's spans.
    pub thread: Thread,
}

impl Turn {
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

/// One span: an API request or a tool call.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Span {
    /// The index, in the same thread, of the span this one is part of: the
    /// request a tool call came from.
    pub parent: Option<usize>,
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
    Request(Request),
    Tool(ToolCall),
}

/// One API response.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct Request {
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
    /// A block of a response arrived at `at`: its request span, made if this
    /// is the first block. `None` when the thread is full.
    pub(crate) fn respond(
        &mut self,
        message_id: Option<&str>,
        started_at: Option<Timestamp>,
        at: Option<Timestamp>,
    ) -> Option<(usize, &mut Request)> {
        // The blocks of one response are written together: it is among the
        // last spans if it is there at all.
        let known = message_id.and_then(|id| {
            self.spans.iter().rposition(|span| {
                matches!(&span.kind, SpanKind::Request(r) if r.message_id.as_deref() == Some(id))
            })
        });
        let index = match known {
            Some(index) => index,
            None => {
                self.push(Span {
                    parent: None,
                    started_at,
                    ended_at: None,
                    kind: SpanKind::Request(Request {
                        message_id: message_id.map(str::to_owned),
                        ..Request::default()
                    }),
                })?;
                self.spans.len() - 1
            }
        };
        let span = &mut self.spans[index];
        span.ended_at = span.ended_at.max(at);
        match &mut span.kind {
            SpanKind::Request(request) => Some((index, request)),
            SpanKind::Tool(_) => None,
        }
    }

    /// A tool call made by the response at `parent`.
    pub(crate) fn call(&mut self, parent: Option<usize>, call: ToolCall) {
        self.push(Span {
            parent,
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

    /// The indices of each span's children, and of the spans without a
    /// parent, in order.
    fn children(&self) -> (Vec<usize>, Vec<Vec<usize>>) {
        let mut roots = Vec::new();
        let mut children = vec![Vec::new(); self.spans.len()];
        for (index, span) in self.spans.iter().enumerate() {
            // A parent is always an earlier span; anything else is a root.
            match span.parent.filter(|parent| *parent < index) {
                Some(parent) => children[parent].push(index),
                None => roots.push(index),
            }
        }
        (roots, children)
    }
}

/// One row of a turn's tree.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Node<'a> {
    /// 0 for what hangs directly under the turn.
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
    /// A turn of this session as a tree, depth first: each request, the tool
    /// calls it made, the subagents those spawned and what they did. Then
    /// the subagents that started during the turn and are not linked to a
    /// tool call that is kept.
    pub fn walk<'a>(&'a self, turn: &'a Turn) -> Vec<Node<'a>> {
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
        walk.thread(&turn.thread, 0);

        // The window the turn covers: up to the next turn's start.
        let next = self
            .trace
            .iter()
            .position(|other| std::ptr::eq(other, turn))
            .and_then(|at| self.trace.get(at + 1))
            .and_then(|next| next.started_at);
        let within = |subagent: &Subagent| match (subagent.started_at, turn.started_at) {
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
            .map(|turn| &turn.thread)
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
        let (roots, children) = thread.children();
        // Depth first without recursion over spans: a stack of what is left.
        let mut stack: Vec<(usize, usize)> = roots.into_iter().rev().map(|i| (i, depth)).collect();
        while let Some((index, depth)) = stack.pop() {
            let span = &thread.spans[index];
            self.nodes.push(Node {
                depth,
                item: Item::Span(span),
            });
            if let SpanKind::Tool(call) = &span.kind {
                for subagent in self
                    .spawned
                    .get(call.id.as_str())
                    .cloned()
                    .unwrap_or_default()
                {
                    self.subagent(subagent, depth + 1);
                }
            }
            stack.extend(
                children[index]
                    .iter()
                    .rev()
                    .map(|child| (*child, depth + 1)),
            );
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
