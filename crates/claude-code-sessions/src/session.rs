//! `Session` and `SessionStore`: the fold of records into state.
//!
//! A [`SessionStore`] holds one [`Session`] per session id. Records go in
//! through two doors:
//!
//! - [`apply`](SessionStore::apply) is the fold. Feed it every record of a
//!   session's transcript, in file order, and the records of its subagent
//!   transcripts (each in its own file order, interleaved with the main
//!   transcript however they arrive). It updates everything.
//! - [`preview`](SessionStore::preview) updates only the snapshot fields:
//!   title, last prompt, branch, model, last activity, context. It is for a
//!   first look at the end of a large transcript
//!   ([`Tail::from_end`](crate::tail::Tail::from_end)) before the full read
//!   catches up.
//!
//! Two kinds of fields:
//!
//! - **Snapshot fields** are last-write-wins by record timestamp, so a preview
//!   followed by the full fold ends in the same place.
//! - **Accumulated fields** (tokens, cost, tool calls, turns) are folded once
//!   per record. A record already folded is recognised and skipped, so
//!   applying the same records again changes nothing.
//!
//! To recognise repeats the store keeps a 64-bit fingerprint per record, and
//! to price the part of a session Claude Code has not reported yet it keeps
//! the time and estimated cost of every response. Those are the only
//! per-record data kept; the conversation itself is not.
//!
//! # Cost
//!
//! Claude Code writes its own running total (`cost-state`) when it exits. It
//! is the better number where it exists: it includes requests the transcript
//! never records. But it exists only up to the last clean exit, and a process
//! that was killed leaves it far behind. So [`Session::cost`] is the larger of
//! "reported total plus the estimate of everything after it" and "the estimate
//! of everything", and [`Cost::source`] says which it was.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::Duration;

use crate::discover::SessionFiles;
use crate::pricing::{self, Pricing, Tokens};
use crate::record::{Record, Timestamp, Usage, View};

/// How many recent tool calls a session keeps unless told otherwise.
pub const DEFAULT_RECENT_TOOLS: usize = 20;

/// How many recent API requests are remembered to attribute responses to a
/// `querySource`. A response's request is written just before it.
const RECENT_REQUESTS: usize = 256;

/// The model name Claude Code gives messages it made up itself.
const SYNTHETIC_MODEL: &str = "<synthetic>";

/// Every session the store has been told about, by session id.
#[derive(Debug, Clone)]
pub struct SessionStore {
    entries: BTreeMap<String, Entry>,
    pricing: Pricing,
    recent_tools: usize,
}

#[derive(Debug, Clone)]
struct Entry {
    session: Session,
    fold: Fold,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionStore {
    /// An empty store pricing with the embedded table.
    pub fn new() -> Self {
        Self::with_pricing(pricing::embedded().clone())
    }

    /// An empty store pricing with `pricing`.
    pub fn with_pricing(pricing: Pricing) -> Self {
        Self {
            entries: BTreeMap::new(),
            pricing,
            recent_tools: DEFAULT_RECENT_TOOLS,
        }
    }

    /// Sets how many recent tool calls each session keeps.
    pub fn with_recent_tools(mut self, limit: usize) -> Self {
        self.recent_tools = limit;
        self
    }

    /// Records that a session exists on disk: its project path and what its
    /// subagents' metadata files say. Safe to call again after every
    /// [`discover`](crate::discover::discover).
    pub fn discovered(&mut self, files: &SessionFiles) {
        let session = &mut self.entry(&files.id).session;
        if files.cwd.is_some() {
            session.project_path.clone_from(&files.cwd);
        }
        for found in &files.subagents {
            let subagent = session.subagent_mut(&found.agent_id);
            if let Some(meta) = &found.meta {
                subagent.agent_type.clone_from(&meta.agent_type);
                subagent.description.clone_from(&meta.description);
                subagent.finished = meta.finished;
            }
        }
    }

    /// Folds one record into the session. See the module documentation for
    /// the order records must arrive in.
    pub fn apply(&mut self, session_id: &str, record: &Record) {
        let limit = self.recent_tools;
        let pricing = &self.pricing;
        let entry = self
            .entries
            .entry(session_id.to_owned())
            .or_insert_with(|| Entry::new(session_id));
        entry.apply(record, pricing, limit, Mode::Fold);
    }

    /// Folds records in the order given.
    pub fn apply_records<'a>(
        &mut self,
        session_id: &str,
        records: impl IntoIterator<Item = &'a Record>,
    ) {
        for record in records {
            self.apply(session_id, record);
        }
    }

    /// Updates only the snapshot fields from one record.
    pub fn preview(&mut self, session_id: &str, record: &Record) {
        let limit = self.recent_tools;
        let pricing = &self.pricing;
        let entry = self
            .entries
            .entry(session_id.to_owned())
            .or_insert_with(|| Entry::new(session_id));
        entry.apply(record, pricing, limit, Mode::Preview);
    }

    /// Previews records in the order given.
    pub fn preview_records<'a>(
        &mut self,
        session_id: &str,
        records: impl IntoIterator<Item = &'a Record>,
    ) {
        for record in records {
            self.preview(session_id, record);
        }
    }

    /// Forgets everything folded from a session's records, keeping what
    /// [`discovered`](Self::discovered) set. Call it when a tail reports that
    /// one of the session's files restarted, then fold all of its files
    /// again.
    pub fn reset(&mut self, session_id: &str) {
        let Some(entry) = self.entries.get_mut(session_id) else {
            return;
        };
        let old = std::mem::replace(entry, Entry::new(session_id));
        entry.session.project_path = old.session.project_path;
        for (id, subagent) in old.session.subagents {
            let kept = entry.session.subagent_mut(&id);
            kept.agent_type = subagent.agent_type;
            kept.description = subagent.description;
            kept.finished = subagent.finished;
        }
    }

    /// Removes a session.
    pub fn remove(&mut self, session_id: &str) -> Option<Session> {
        self.entries.remove(session_id).map(|e| e.session)
    }

    /// One session.
    pub fn get(&self, session_id: &str) -> Option<&Session> {
        self.entries.get(session_id).map(|e| &e.session)
    }

    /// Every session, ordered by id.
    pub fn sessions(&self) -> impl Iterator<Item = &Session> {
        self.entries.values().map(|e| &e.session)
    }

    /// How many sessions the store holds.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no session.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn entry(&mut self, session_id: &str) -> &mut Entry {
        self.entries
            .entry(session_id.to_owned())
            .or_insert_with(|| Entry::new(session_id))
    }
}

/// One Claude Code session.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Session {
    pub id: String,
    /// Where the session belongs, as resolved by
    /// [`discover`](crate::discover): stable for the session's life.
    pub project_path: Option<PathBuf>,
    /// The working directory of the latest record. It moves when the session
    /// changes directory.
    pub cwd: Option<PathBuf>,
    pub git_branch: Option<String>,
    /// The Claude Code version that wrote the latest record.
    pub version: Option<String>,
    pub title: Option<String>,
    /// The prompt the user typed last.
    pub last_prompt: Option<String>,
    /// The model of the latest main-thread response.
    pub model: Option<String>,
    pub started_at: Option<Timestamp>,
    /// The latest record, subagents included.
    pub last_activity: Option<Timestamp>,
    /// The transcript ends with Claude Code's exit bookkeeping: the process
    /// that ran the session is gone (until someone resumes it).
    pub exited: bool,
    /// What the main thread's context held at its latest response.
    pub context: Option<Context>,
    pub usage: UsageTotals,
    pub cost: Cost,
    /// Completed turns.
    pub turns: u64,
    /// Tool calls made, subagents' included.
    pub tool_calls: u64,
    /// The latest tool calls, oldest first, at most the store's limit.
    pub recent_tools: VecDeque<ToolCall>,
    /// The session's subagents by agent id.
    pub subagents: BTreeMap<String, Subagent>,
}

impl Session {
    fn new(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            project_path: None,
            cwd: None,
            git_branch: None,
            version: None,
            title: None,
            last_prompt: None,
            model: None,
            started_at: None,
            last_activity: None,
            exited: false,
            context: None,
            usage: UsageTotals::default(),
            cost: Cost::default(),
            turns: 0,
            tool_calls: 0,
            recent_tools: VecDeque::new(),
            subagents: BTreeMap::new(),
        }
    }

    /// What the transcript alone can say about the session at `now`. It
    /// cannot tell "waiting for input" from "thinking"; only live events can.
    pub fn activity(&self, now: Timestamp, active_within: Duration) -> Activity {
        if self.exited {
            return Activity::Exited;
        }
        let Some(last) = self.last_activity else {
            return Activity::Idle;
        };
        let quiet_ms = now.as_millis().saturating_sub(last.as_millis()).max(0);
        let quiet = Duration::from_millis(quiet_ms as u64);
        if quiet <= active_within {
            Activity::Active { quiet }
        } else {
            Activity::Idle
        }
    }

    fn subagent_mut(&mut self, agent_id: &str) -> &mut Subagent {
        self.subagents
            .entry(agent_id.to_owned())
            .or_insert_with(|| Subagent {
                agent_id: agent_id.to_owned(),
                ..Subagent::default()
            })
    }
}

/// What a transcript says a session is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// A record was written within the window; `quiet` is how long ago.
    Active { quiet: Duration },
    /// Nothing recent, and no sign that the process exited.
    Idle,
    /// Claude Code exited.
    Exited,
}

/// How full the context is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Context {
    /// Input, cache writes and cache reads of the latest response (or what a
    /// compaction left).
    pub tokens: u64,
    /// The model's largest context window, when the model is known. Claude
    /// Code may compact well before it.
    pub window: Option<u64>,
}

impl Context {
    /// `tokens / window`, when the window is known.
    pub fn fill(&self) -> Option<f64> {
        self.window
            .filter(|w| *w > 0)
            .map(|w| self.tokens as f64 / w as f64)
    }
}

/// Tokens by model and by what asked for them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageTotals {
    /// By `message.model`.
    pub by_model: BTreeMap<String, ModelUsage>,
    /// By `querySource` (`repl_main_thread`, `agent:builtin:Explore`, …).
    /// Responses whose request is not recorded are under `unknown`.
    pub by_query_source: BTreeMap<String, ModelUsage>,
}

impl UsageTotals {
    /// Everything together.
    pub fn total(&self) -> ModelUsage {
        let mut total = ModelUsage::default();
        for usage in self.by_model.values() {
            total.add(usage);
        }
        total
    }
}

/// Token counts and their estimated price.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ModelUsage {
    /// API responses counted.
    pub responses: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_write_5m_tokens: u64,
    pub cache_write_1h_tokens: u64,
    pub cache_read_tokens: u64,
    /// The price-table estimate for the responses that could be priced.
    pub estimated_usd: f64,
    /// Responses of a model the price table does not know.
    pub unpriced_responses: u64,
}

impl ModelUsage {
    /// The estimate, or `None` when nothing here could be priced: an unknown
    /// model costs "unknown", never zero.
    pub fn estimated_cost(&self) -> Option<f64> {
        (self.responses == 0 || self.unpriced_responses < self.responses)
            .then_some(self.estimated_usd)
    }

    /// Everything the model was given and produced.
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            + self.output_tokens
            + self.cache_write_5m_tokens
            + self.cache_write_1h_tokens
            + self.cache_read_tokens
    }

    fn add(&mut self, other: &ModelUsage) {
        self.responses += other.responses;
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_write_5m_tokens += other.cache_write_5m_tokens;
        self.cache_write_1h_tokens += other.cache_write_1h_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.estimated_usd += other.estimated_usd;
        self.unpriced_responses += other.unpriced_responses;
    }

    fn count(&mut self, tokens: &Tokens, usd: Option<f64>) {
        self.responses += 1;
        self.input_tokens += tokens.input;
        self.output_tokens += tokens.output;
        self.cache_write_5m_tokens += tokens.cache_write_5m;
        self.cache_write_1h_tokens += tokens.cache_write_1h;
        self.cache_read_tokens += tokens.cache_read;
        match usd {
            Some(usd) => self.estimated_usd += usd,
            None => self.unpriced_responses += 1,
        }
    }
}

/// What a session has cost.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Cost {
    /// `None` when there is nothing to go on: no report and no response that
    /// could be priced.
    pub usd: Option<f64>,
    pub source: CostSource,
    /// Some responses were of a model the price table does not know and are
    /// missing from an estimated part.
    pub incomplete: bool,
}

/// Where a [`Cost`] came from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CostSource {
    /// Nothing to go on.
    #[default]
    Unknown,
    /// Claude Code's own total, and nothing happened after it.
    Reported,
    /// Claude Code's total plus the estimate of what happened after it.
    ReportedPlusEstimate,
    /// The price-table estimate: Claude Code has reported nothing, or
    /// reported less than the transcript accounts for.
    Estimated,
}

/// One tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// The subagent that made the call, if one did.
    pub agent_id: Option<String>,
    pub started_at: Option<Timestamp>,
    /// From the call to its result. `None` while it runs.
    pub duration_ms: Option<u64>,
    /// `None` until the result is seen.
    pub success: Option<bool>,
}

/// One subagent of a session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Subagent {
    pub agent_id: String,
    /// `Explore`, `general-purpose`, … (from the metadata file).
    pub agent_type: Option<String>,
    /// The task the parent gave it (from the metadata file).
    pub description: Option<String>,
    /// From the metadata file; absent while the agent runs.
    pub finished: Option<bool>,
    pub model: Option<String>,
    pub started_at: Option<Timestamp>,
    pub last_activity: Option<Timestamp>,
    pub usage: ModelUsage,
    pub tool_calls: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Fold,
    Preview,
}

/// The bookkeeping behind a session that is not part of its public state.
#[derive(Debug, Clone, Default)]
struct Fold {
    /// Fingerprints of what has been folded.
    seen: HashSet<u64>,
    /// Recent `api-request` ids and their `querySource`.
    requests: VecDeque<(String, String)>,
    /// When each response happened and what it is estimated to have cost.
    charges: Vec<Charge>,
    /// The sum of `charges`.
    estimate: f64,
    reported: Option<Reported>,
    /// The sum of the charges that happened after `reported`.
    since_report: f64,
    unpriced: bool,
    /// The latest timestamp of the main transcript, in file order.
    main_cursor: Option<Timestamp>,
    snapshot_at: Option<Timestamp>,
    model_at: Option<Timestamp>,
    context_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy)]
struct Charge {
    at: Option<Timestamp>,
    usd: f64,
}

/// Claude Code's own total and where in the transcript it was written.
#[derive(Debug, Clone, Copy)]
struct Reported {
    usd: f64,
    after: Option<Timestamp>,
}

impl Entry {
    fn new(session_id: &str) -> Self {
        Self {
            session: Session::new(session_id),
            fold: Fold::default(),
        }
    }

    fn apply(&mut self, record: &Record, pricing: &Pricing, limit: usize, mode: Mode) {
        let common = record.common();
        let agent = common.agent_id.as_deref();
        let at = common.timestamp;
        let session = &mut self.session;
        let fold = &mut self.fold;

        if let Some(at) = at {
            session.started_at = Some(session.started_at.map_or(at, |s| s.min(at)));
            session.last_activity = Some(session.last_activity.map_or(at, |l| l.max(at)));
        }
        match agent {
            None => {
                if let Some(at) = at {
                    if newer(&mut fold.snapshot_at, at) {
                        if common.cwd.is_some() {
                            session.cwd.clone_from(&common.cwd);
                        }
                        if common.git_branch.is_some() {
                            session.git_branch.clone_from(&common.git_branch);
                        }
                        if common.version.is_some() {
                            session.version.clone_from(&common.version);
                        }
                    }
                    if mode == Mode::Fold {
                        fold.main_cursor = Some(fold.main_cursor.map_or(at, |c| c.max(at)));
                    }
                }
            }
            Some(agent) => {
                if let Some(at) = at {
                    let subagent = session.subagent_mut(agent);
                    subagent.started_at = Some(subagent.started_at.map_or(at, |s| s.min(at)));
                    subagent.last_activity = Some(subagent.last_activity.map_or(at, |l| l.max(at)));
                }
            }
        }

        // A record with a uuid is folded once, whatever it is.
        let first_time = mode == Mode::Fold
            && common
                .uuid
                .as_deref()
                .is_none_or(|uuid| fold.seen.insert(fingerprint("record", uuid)));

        match record.view() {
            View::Title(title) => {
                if title.title.is_some() {
                    session.title.clone_from(&title.title);
                }
            }
            View::LastPrompt(prompt) => {
                if prompt.text.is_some() {
                    session.last_prompt.clone_from(&prompt.text);
                }
            }
            View::Assistant(assistant) => {
                let model = assistant.model.as_deref().filter(|m| *m != SYNTHETIC_MODEL);
                match agent {
                    None => {
                        session.exited = false;
                        if let (Some(model), Some(at)) = (model, at) {
                            if newer(&mut fold.model_at, at) {
                                session.model = Some(model.to_owned());
                            }
                        }
                        if let (Some(model), Some(usage), Some(at)) = (model, assistant.usage, at) {
                            if newer(&mut fold.context_at, at) {
                                session.context = Some(Context {
                                    tokens: usage.context_tokens(),
                                    window: pricing.model(model).map(|m| m.context_window),
                                });
                            }
                        }
                    }
                    Some(agent) => {
                        if let Some(model) = model {
                            session.subagent_mut(agent).model = Some(model.to_owned());
                        }
                    }
                }
                if !first_time {
                    return;
                }

                // One response is several records; its usage counts once.
                let counted = assistant
                    .message_id
                    .as_deref()
                    .is_none_or(|id| fold.seen.insert(fingerprint("message", id)));
                if let (true, Some(model), Some(usage)) = (counted, model, assistant.usage) {
                    let tokens = tokens_of(&usage);
                    let usd = pricing.model(model).map(|m| m.cost(&tokens, usage.fast));
                    match usd {
                        Some(usd) => fold.charge(Charge { at, usd }),
                        None => fold.unpriced = true,
                    }
                    let source = assistant
                        .request_ref
                        .as_deref()
                        .and_then(|id| fold.requests.iter().rev().find(|(r, _)| r == id))
                        .map_or("unknown", |(_, source)| source.as_str())
                        .to_owned();
                    session
                        .usage
                        .by_model
                        .entry(model.to_owned())
                        .or_default()
                        .count(&tokens, usd);
                    session
                        .usage
                        .by_query_source
                        .entry(source)
                        .or_default()
                        .count(&tokens, usd);
                    if let Some(agent) = agent {
                        session.subagent_mut(agent).usage.count(&tokens, usd);
                    }
                }

                for call in &assistant.tool_uses {
                    let (Some(id), Some(name)) = (&call.id, &call.name) else {
                        continue;
                    };
                    session.tool_calls += 1;
                    if let Some(agent) = agent {
                        session.subagent_mut(agent).tool_calls += 1;
                    }
                    session.recent_tools.push_back(ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        agent_id: agent.map(str::to_owned),
                        started_at: at,
                        duration_ms: None,
                        success: None,
                    });
                    while session.recent_tools.len() > limit {
                        session.recent_tools.pop_front();
                    }
                }
            }
            View::User(user) => {
                if agent.is_none() {
                    session.exited = false;
                }
                if !first_time {
                    return;
                }
                for result in &user.tool_results {
                    let call = result
                        .tool_use_id
                        .as_deref()
                        .and_then(|id| session.recent_tools.iter_mut().rev().find(|c| c.id == id));
                    if let Some(call) = call {
                        call.success = Some(result.success());
                        call.duration_ms = match (call.started_at, at) {
                            (Some(start), Some(end)) => {
                                Some(end.as_millis().saturating_sub(start.as_millis()).max(0) as u64)
                            }
                            _ => None,
                        };
                    }
                }
            }
            View::System(system) => {
                if agent.is_none() {
                    if let (Some(compact), Some(at)) = (&system.compact, at) {
                        if let Some(left) = compact.post_tokens {
                            if newer(&mut fold.context_at, at) {
                                let window = session.context.and_then(|c| c.window);
                                session.context = Some(Context {
                                    tokens: left,
                                    window,
                                });
                            }
                        }
                    }
                }
                if first_time
                    && agent.is_none()
                    && system.subtype.as_deref() == Some("turn_duration")
                {
                    session.turns += 1;
                }
            }
            View::ApiRequest(request) => {
                if mode == Mode::Fold {
                    if let (Some(id), Some(source)) = (&request.id, &request.query_source) {
                        fold.requests.push_back((id.clone(), source.clone()));
                        while fold.requests.len() > RECENT_REQUESTS {
                            fold.requests.pop_front();
                        }
                    }
                }
            }
            View::CostState(state) => {
                session.exited = true;
                if mode == Mode::Fold {
                    // The record has no id of its own. Its content (totals and
                    // durations) identifies the report.
                    let key = record.raw().to_string();
                    if let (Some(usd), true) = (
                        state.total_cost_usd,
                        fold.seen.insert(fingerprint("cost", &key)),
                    ) {
                        if fold.reported.is_none_or(|r| usd >= r.usd) {
                            fold.report(Reported {
                                usd,
                                after: fold.main_cursor,
                            });
                        }
                    }
                }
            }
            _ => {}
        }

        if mode == Mode::Fold {
            session.cost = fold.cost();
        }
    }
}

impl Fold {
    fn charge(&mut self, charge: Charge) {
        self.estimate += charge.usd;
        if self.reported.is_some_and(|r| charge.is_after(&r)) {
            self.since_report += charge.usd;
        }
        self.charges.push(charge);
    }

    fn report(&mut self, reported: Reported) {
        self.since_report = self
            .charges
            .iter()
            .filter(|c| c.is_after(&reported))
            .map(|c| c.usd)
            .sum();
        self.reported = Some(reported);
    }

    fn cost(&self) -> Cost {
        let estimated = Cost {
            usd: (!self.charges.is_empty()).then_some(self.estimate),
            source: if self.charges.is_empty() {
                CostSource::Unknown
            } else {
                CostSource::Estimated
            },
            incomplete: self.unpriced,
        };
        let Some(reported) = self.reported else {
            return estimated;
        };
        let combined = reported.usd + self.since_report;
        if self.estimate > combined {
            // Claude Code reported less than the transcript accounts for.
            estimated
        } else if self.since_report > 0.0 {
            Cost {
                usd: Some(combined),
                source: CostSource::ReportedPlusEstimate,
                incomplete: self.unpriced,
            }
        } else {
            Cost {
                usd: Some(reported.usd),
                source: CostSource::Reported,
                incomplete: false,
            }
        }
    }
}

impl Charge {
    /// Whether the response happened after the report: later than the last
    /// main-transcript record that preceded it. Subagents' responses are
    /// placed the same way, by their own timestamps.
    fn is_after(&self, reported: &Reported) -> bool {
        match (self.at, reported.after) {
            (Some(at), Some(after)) => at > after,
            (_, None) => true,
            (None, Some(_)) => false,
        }
    }
}

/// Moves `latest` forward to `at` and says whether `at` is the newest so far.
fn newer(latest: &mut Option<Timestamp>, at: Timestamp) -> bool {
    if latest.is_some_and(|l| at < l) {
        return false;
    }
    *latest = Some(at);
    true
}

fn tokens_of(usage: &Usage) -> Tokens {
    let one_hour = usage.cache_creation_1h_input_tokens.unwrap_or(0);
    Tokens {
        input: usage.input_tokens,
        output: usage.output_tokens,
        // Without the breakdown every write is taken as a 5 minute one.
        cache_write_5m: usage.cache_creation_input_tokens.saturating_sub(one_hour),
        cache_write_1h: one_hour,
        cache_read: usage.cache_read_input_tokens,
    }
}

fn fingerprint(kind: &str, id: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    kind.hash(&mut hasher);
    id.hash(&mut hasher);
    hasher.finish()
}
