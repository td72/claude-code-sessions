import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as api from "./api.ts";
import type { Interaction, Node, SessionSummary, Trace, Whole } from "./api.ts";
import * as format from "./format.ts";

/** How often the times on the page ("12s") are written again. */
const TICK_MS = 5000;

export default function App() {
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [trace, setTrace] = useState<Trace | null>(null);
  const [problem, setProblem] = useState<"unauthorized" | "unreachable" | null>(null);
  const [now, setNow] = useState(() => Date.now());
  // The session the trace was last asked for: an answer for another is dropped.
  const wanted = useRef<string | null>(null);

  const failed = useCallback((error: unknown) => {
    if (error instanceof api.Unauthorized) setProblem("unauthorized");
    else if (!(error instanceof api.NotFound)) setProblem("unreachable");
  }, []);

  const loadTrace = useCallback(
    async (id: string | null) => {
      wanted.current = id;
      if (id === null) return setTrace(null);
      try {
        const loaded = await api.trace(id);
        if (wanted.current === id) setTrace(loaded);
      } catch (error) {
        if (error instanceof api.NotFound && wanted.current === id) setTrace(null);
        failed(error);
      }
    },
    [failed],
  );

  /** Shows a session: its row is marked, and its trace is read. */
  const select = useCallback(
    (id: string) => {
      setSelected(id);
      void loadTrace(id);
    },
    [loadTrace],
  );

  const loadSessions = useCallback(async () => {
    try {
      const listed = await api.sessions();
      setSessions(listed.sessions);
      setNotice(listed.notice);
      setProblem(null);
      // Until one is chosen, the latest session is the one shown.
      const first = listed.sessions[0]?.id;
      if (wanted.current === null && first !== undefined) select(first);
      else void loadTrace(wanted.current);
    } catch (error) {
      failed(error);
    }
  }, [failed, select, loadTrace]);

  // The server says when something changed, and says so once when the
  // stream opens: what is shown is read then, and again each time.
  useEffect(() => {
    const events = new EventSource("/api/events");
    const changed = () => void loadSessions();
    events.addEventListener("changed", changed);
    return () => events.close();
  }, [loadSessions]);

  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), TICK_MS);
    return () => clearInterval(timer);
  }, []);

  if (problem === "unauthorized") {
    return (
      <main className="message">
        <h1>iktinos</h1>
        <p>
          This browser has not been let in. Open the URL <code>iktinos serve</code> printed when it
          started: it carries the token.
        </p>
      </main>
    );
  }

  const counts = { busy: 0, needs_approval: 0, waiting: 0 };
  for (const session of sessions) {
    const kind = session.status.kind;
    if (kind === "busy" || kind === "needs_approval" || kind === "waiting") counts[kind] += 1;
  }

  return (
    <div className="app">
      <header>
        <h1>iktinos</h1>
        <span>
          {sessions.length} {sessions.length === 1 ? "session" : "sessions"}
        </span>
        <span className="busy">{counts.busy} busy</span>
        {counts.needs_approval > 0 && (
          <span className="needs_approval">{counts.needs_approval} need approval</span>
        )}
        {counts.waiting > 0 && <span className="waiting">{counts.waiting} waiting</span>}
        {problem === "unreachable" && <span className="error">the server does not answer</span>}
        {notice && <span className="error">{notice}</span>}
      </header>
      <SessionList sessions={sessions} selected={selected} onSelect={select} now={now} />
      {trace ? (
        <TraceView key={trace.session.id} trace={trace} now={now} />
      ) : (
        <section className="trace empty">
          {sessions.length === 0 ? "no sessions" : "choose a session"}
        </section>
      )}
    </div>
  );
}

function SessionList(props: {
  sessions: SessionSummary[];
  selected: string | null;
  onSelect: (id: string) => void;
  now: number;
}) {
  return (
    <nav className="sessions">
      <table>
        <thead>
          <tr>
            <th>session</th>
            <th>state</th>
            <th className="number">updated</th>
            <th className="number">tokens</th>
            <th className="number">cost</th>
          </tr>
        </thead>
        <tbody>
          {props.sessions.map((session) => (
            <tr
              key={session.id}
              className={session.id === props.selected ? "selected" : undefined}
              onClick={() => props.onSelect(session.id)}
            >
              <td>
                <div className="cwd">{session.location_name ?? "—"}</div>
                <div className="label">{session.label}</div>
              </td>
              <td className={session.status.kind}>{session.status.text}</td>
              <td className="number">{format.ago(props.now, session.last_seen)}</td>
              <td className="number">{format.tokens(session.tokens)}</td>
              <td className="number">{format.cost(session.cost)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </nav>
  );
}

/** What names an interaction or a node across refreshes. */
const interactionKey = (interaction: Interaction, index: number) =>
  interaction.id || `at-${interaction.started_at ?? index}`;

function nodeKey(node: Node, index: number): string {
  switch (node.kind) {
    case "tool":
      return `t:${node.id}`;
    case "subagent":
      return `a:${node.agent_id}`;
    case "llm_request":
      return `r:${node.message_id ?? index}:${node.started_at ?? 0}`;
  }
}

function TraceView({ trace, now }: { trace: Trace; now: number }) {
  const session = trace.session;
  // The interaction that is open, and the row chosen in it. Until one is
  // chosen, the latest interaction is the one shown.
  const [open, setOpen] = useState<string | null>(null);
  const [chosen, setChosen] = useState<string | null>(null);
  const first = trace.interactions[0];
  const openKey = open ?? (first ? interactionKey(first, 0) : null);

  return (
    <section className="trace">
      <div className="heading">
        <span className="cwd">{session.location ?? "—"}</span>
        <span className="label">{session.label}</span>
        {session.branch && <span className="dim">{session.branch}</span>}
        <span className={session.status.kind}>{session.status.text}</span>
        <span className="dim">{format.modelName(session.model)}</span>
      </div>
      {trace.interactions.length === 0 && (
        <p className="dim">nothing was read of this session's interactions</p>
      )}
      {trace.interactions.map((interaction, index) => {
        const key = interactionKey(interaction, index);
        return (
          <InteractionView
            key={key}
            session={session.id}
            interaction={interaction}
            open={key === openKey}
            onToggle={() => {
              setOpen(key === openKey ? "" : key);
              setChosen(null);
            }}
            chosen={key === openKey ? chosen : null}
            onChoose={setChosen}
            now={now}
          />
        );
      })}
    </section>
  );
}

function InteractionView(props: {
  session: string;
  interaction: Interaction;
  open: boolean;
  onToggle: () => void;
  chosen: string | null;
  onChoose: (key: string) => void;
  now: number;
}) {
  const { interaction, now } = props;
  const requests = interaction.nodes.filter((n) => n.depth === 0 && n.kind === "llm_request");
  const tools = interaction.nodes.filter((n) => n.kind === "tool" && n.agent_id === null);
  // The time axis: from the interaction's start to its end, or to now while
  // it runs, or to the last thing known of it.
  const start = interaction.started_at;
  const end = useMemo(() => {
    if (interaction.ended_at !== null) return interaction.ended_at;
    if (interaction.running) return now;
    const known = interaction.nodes.flatMap((n) => [n.ended_at, n.started_at]);
    return Math.max(start ?? 0, ...known.filter((at): at is number => at !== null));
  }, [interaction, now, start]);
  const took = interaction.duration_ms ?? (start !== null ? Math.max(0, end - start) : null);
  const chosen = interaction.nodes.find((node, index) => nodeKey(node, index) === props.chosen);

  return (
    <article className={props.open ? "interaction open" : "interaction"}>
      <button type="button" className="summary" onClick={props.onToggle}>
        <span className="fold">{props.open ? "▾" : "▸"}</span>
        <span className="dim">{format.when(now, start)}</span>
        <span className="prompt">
          {interaction.prompt ?? <span className="dim">(the prompt was not read)</span>}
        </span>
        <span className="dim">
          {requests.length} llm · {tools.length} tools
        </span>
        <span className="number">{format.duration(took)}</span>
        <span className={interaction.running ? "warn" : interaction.ended_at ? "ok" : "dim"}>
          {interaction.running ? "running" : interaction.ended_at ? "done" : "no end"}
        </span>
      </button>
      {props.open && (
        <div className="body">
          {interaction.nodes.length === 0 && (
            <p className="nothing dim">nothing was read of this interaction but its prompt</p>
          )}
          <ol className="tree">
            {interaction.nodes.map((node, index) => {
              const key = nodeKey(node, index);
              return (
                <NodeRow
                  key={key}
                  node={node}
                  nodes={interaction.nodes}
                  chosen={key === props.chosen}
                  onChoose={() => props.onChoose(key)}
                  window={start !== null ? [start, end] : null}
                  running={interaction.running}
                  now={now}
                />
              );
            })}
            {interaction.dropped > 0 && (
              <li className="dim">{interaction.dropped} more spans were not kept</li>
            )}
          </ol>
          {chosen && <Detail session={props.session} node={chosen} nodes={interaction.nodes} />}
        </div>
      )}
    </article>
  );
}

/** The tools a response called, by name, in order. */
function called(node: Node, nodes: Node[]): string[] {
  if (node.kind !== "llm_request") return [];
  const at = nodes.indexOf(node);
  const names: string[] = [];
  // Its calls follow it, one level deeper, until the next row of its level.
  for (const next of nodes.slice(at + 1)) {
    if (next.depth <= node.depth) break;
    if (next.depth === node.depth + 1 && next.kind === "tool")
      names.push(format.toolName(next.name));
  }
  return names;
}

function label(node: Node, nodes: Node[]): { name: string; beside: string } {
  switch (node.kind) {
    case "tool":
      return { name: format.toolName(node.name), beside: node.summary ?? "" };
    case "subagent":
      return {
        name: `↳ ${node.agent_type ?? "agent"}`,
        beside: node.description ?? node.agent_id,
      };
    case "llm_request": {
      if (node.text) return { name: node.text, beside: "" };
      const parts = [];
      if (node.thinking_ms !== null) parts.push(`thinking ${format.duration(node.thinking_ms)}`);
      const tools = called(node, nodes);
      if (tools.length > 0) parts.push(`→ ${tools.join(", ")}`);
      return { name: parts.join(" ") || "llm_request", beside: "" };
    }
  }
}

function state(node: Node, running: boolean): { text: string; className: string } {
  switch (node.kind) {
    case "tool":
      if (node.success === true) return { text: "ok", className: "ok" };
      if (node.success === false) return { text: "failed", className: "error" };
      return running
        ? { text: "running", className: "warn" }
        : { text: "no result", className: "dim" };
    case "subagent":
      return node.finished
        ? { text: "done", className: "ok" }
        : { text: "running", className: "warn" };
    case "llm_request": {
      const stop = node.stop_reason ?? "";
      const className =
        stop === "end_turn" ? "ok" : stop === "tool_use" || stop === "" ? "dim" : "error";
      return { text: stop, className };
    }
  }
}

function NodeRow(props: {
  node: Node;
  nodes: Node[];
  chosen: boolean;
  onChoose: () => void;
  window: [number, number] | null;
  running: boolean;
  now: number;
}) {
  const { node } = props;
  const { name, beside } = label(node, props.nodes);
  const { text, className } = state(node, props.running);
  const marks = node.kind === "llm_request" ? requestMarks(node) : [];
  // What has not ended reaches to now while the interaction runs.
  const end = node.ended_at ?? (props.running ? props.now : node.started_at);
  const took = node.started_at !== null && end !== null ? Math.max(0, end - node.started_at) : null;
  let bar: { left: string; width: string } | null = null;
  if (props.window && node.started_at !== null && end !== null) {
    const [from, to] = props.window;
    const span = Math.max(1, to - from);
    const left = Math.min(1, Math.max(0, (node.started_at - from) / span));
    const width = Math.min(1 - left, Math.max(0, (end - node.started_at) / span));
    bar = { left: `${left * 100}%`, width: `max(2px, ${width * 100}%)` };
  }
  return (
    <li className={props.chosen ? "chosen" : undefined}>
      <button type="button" onClick={props.onChoose}>
        <span className="what" style={{ paddingLeft: `${node.depth * 1.25}rem` }}>
          {marks.length > 0 && <span className="warn">{marks.join(", ")} </span>}
          <span className={node.kind}>{name}</span>
          {beside && <span className="beside"> {beside}</span>}
        </span>
        <span className={`number ${slow(took)}`}>{format.duration(took)}</span>
        <span className={className}>{text}</span>
        <span className="axis">
          {bar && <span className={`bar ${node.kind} ${className}`} style={bar} />}
        </span>
      </button>
    </li>
  );
}

/** What sets a request apart from the one before it, where anything does. */
function requestMarks(node: Node & { kind: "llm_request" }): string[] {
  const marks = [];
  if (node.rebuilt) marks.push("context rebuilt");
  if (node.reshaped) marks.push("shape changed");
  return marks;
}

/** The slow stand out: ten seconds, a minute. */
function slow(ms: number | null): string {
  if (ms === null) return "";
  return ms >= 60_000 ? "error" : ms >= 10_000 ? "warn" : "";
}

function Detail({ session, node, nodes }: { session: string; node: Node; nodes: Node[] }) {
  switch (node.kind) {
    case "tool":
      return (
        <aside className="detail">
          <h2 className="tool">{format.toolName(node.name)}</h2>
          <Field name="took" value={format.duration(node.duration_ms)} />
          {node.agent_id && <Field name="agent" value={node.agent_id} />}
          <Field name="id" value={node.id} />
          {(node.input ?? node.summary) && (
            <pre className="command">{node.input ?? node.summary}</pre>
          )}
          <OutputView session={session} call={node} />
        </aside>
      );
    case "subagent":
      return (
        <aside className="detail">
          <h2 className="subagent">{node.agent_type ?? "agent"}</h2>
          {node.description && <p>{node.description}</p>}
          <Field name="model" value={node.model ?? "—"} />
          <Field name="tokens" value={format.tokens(node.tokens)} />
          <Field
            name="cost"
            value={node.estimated_usd === null ? "—" : `~${format.usd(node.estimated_usd)}`}
          />
          <Field name="calls" value={String(node.tool_calls)} />
          <Field name="id" value={node.agent_id} />
        </aside>
      );
    case "llm_request": {
      const tools = called(node, nodes);
      return (
        <aside className="detail">
          <h2 className="llm_request">LLM request</h2>
          <Field name="model" value={node.model ?? "—"} />
          {node.query_source && <Field name="source" value={node.query_source} />}
          {node.stop_reason && <Field name="stop" value={node.stop_reason} />}
          {node.rebuilt && (
            <Field name="context" value="rebuilt: sent anew, not as a continuation" warn />
          )}
          {node.reshaped && (
            <Field name="shape" value="the system prompt or the tools changed" warn />
          )}
          {node.thinking_ms !== null && (
            <Field name="thinking" value={`${format.duration(node.thinking_ms)} (approximate)`} />
          )}
          <Field name="took" value={`${format.duration(node.duration_ms)} (approximate)`} />
          {node.input_tokens !== null && (
            <>
              <Field name="input" value={format.tokens(node.input_tokens)} />
              <Field name="output" value={format.tokens(node.output_tokens ?? 0)} />
              <Field
                name="cache"
                value={`${format.tokens(node.cache_read_tokens ?? 0)} read, ${format.tokens(node.cache_write_tokens ?? 0)} written`}
              />
              <Field
                name="cost"
                value={node.usd === null ? "unknown model" : `~${format.usd(node.usd)}`}
              />
            </>
          )}
          {tools.length > 0 && <Field name="called" value={tools.join(", ")} />}
          {node.message_id && (
            <SaidView session={session} message={node.message_id} first={node.text} />
          )}
        </aside>
      );
    }
  }
}

function Field({ name, value, warn }: { name: string; value: string; warn?: boolean }) {
  return (
    <div className="field">
      <span className="name">{name}</span>
      <span className={warn ? "warn" : undefined}>{value}</span>
    </div>
  );
}

/** Something read again on demand: not asked, being read, read, or failed. */
type Read =
  | { state: "excerpt" }
  | { state: "reading" }
  | { state: "whole"; whole: Whole }
  | { state: "failed" };

function useWhole(read: () => Promise<Whole>) {
  const [value, setValue] = useState<Read>({ state: "excerpt" });
  const toggle = () => {
    if (value.state === "whole") return setValue({ state: "excerpt" });
    setValue({ state: "reading" });
    read().then(
      (whole) => setValue({ state: "whole", whole }),
      () => setValue({ state: "failed" }),
    );
  };
  return [value, toggle] as const;
}

function OutputView({ session, call }: { session: string; call: Node & { kind: "tool" } }) {
  const [read, toggle] = useWhole(() => api.wholeOutput(session, call.id));
  const excerpt = call.output;
  if (call.success === null) return <p className="dim">no result yet</p>;
  const shown = read.state === "whole" ? read.whole : excerpt;
  return (
    <div className="output">
      <div className="field">
        <span className="name">output</span>
        <span className="dim">
          {read.state === "whole"
            ? "all of it"
            : excerpt
              ? `${excerpt.lines} lines, ${excerpt.chars} characters${excerpt.truncated ? ": the start" : ""}`
              : "not kept"}
        </span>
        <button type="button" className="link" onClick={toggle} disabled={read.state === "reading"}>
          {read.state === "whole"
            ? "the start"
            : read.state === "reading"
              ? "reading…"
              : "all of it"}
        </button>
      </div>
      {read.state === "failed" && (
        <p className="error">the session's files hold no result of this call</p>
      )}
      {shown && <pre>{shown.text}</pre>}
      {shown?.stderr && (
        <>
          <div className="error">stderr</div>
          <pre>{shown.stderr}</pre>
        </>
      )}
      {shown?.interrupted && <p className="warn">interrupted</p>}
      {shown && shown.other_blocks > 0 && (
        <p className="dim">and {shown.other_blocks} blocks that are not text</p>
      )}
      {shown?.truncated && read.state === "whole" && (
        <p className="dim">… cut where reading stopped</p>
      )}
    </div>
  );
}

function SaidView(props: { session: string; message: string; first: string | null }) {
  const [read, toggle] = useWhole(() => api.wholeResponse(props.session, props.message));
  return (
    <div className="output">
      <div className="field">
        <span className="name">said</span>
        <span className="dim">
          {read.state === "whole" ? "all of it" : props.first ? "the first line" : "nothing"}
        </span>
        {props.first && (
          <button
            type="button"
            className="link"
            onClick={toggle}
            disabled={read.state === "reading"}
          >
            {read.state === "whole"
              ? "the first line"
              : read.state === "reading"
                ? "reading…"
                : "all of it"}
          </button>
        )}
      </div>
      {read.state === "failed" && (
        <p className="error">the session's files hold no text of this response</p>
      )}
      {read.state === "whole" ? (
        <pre>{read.whole.text}</pre>
      ) : (
        props.first && <pre>{props.first}</pre>
      )}
    </div>
  );
}
