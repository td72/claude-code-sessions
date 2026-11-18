// What `iktinos serve` returns (crates/iktinos-web/src/json.rs). Times are
// milliseconds since the Unix epoch.

export type StatusKind = "busy" | "needs_approval" | "waiting" | "idle" | "exited";

export type SessionSummary = {
  id: string;
  label: string;
  location: string | null;
  location_name: string | null;
  branch: string | null;
  model: string | null;
  status: { kind: StatusKind; text: string };
  started_at: number | null;
  last_seen: number | null;
  tokens: number;
  cost: { usd: number | null; source: string; incomplete: boolean };
  interactions: number;
  tool_calls: number;
  subagents: number;
};

export type Usage = {
  name: string;
  responses: number;
  input_tokens: number;
  output_tokens: number;
  cache_write_tokens: number;
  cache_read_tokens: number;
  estimated_usd: number | null;
};

/** One session, with what its detail shows. */
export type SessionDetail = SessionSummary & {
  /** What the running tool is doing, where live events say. */
  running: string | null;
  context: { tokens: number; window: number | null; fill: number | null } | null;
  by_model: Usage[];
  by_source: Usage[];
  rate_limits: { kind: string; percent_used: number; resets_at: string | null }[];
};

export type Output = {
  text: string;
  stderr: string | null;
  truncated: boolean;
  chars: number;
  lines: number;
  interrupted: boolean;
  other_blocks: number;
};

type Timed = {
  depth: number;
  started_at: number | null;
  ended_at: number | null;
  duration_ms: number | null;
};

export type RequestNode = Timed & {
  kind: "llm_request";
  message_id: string | null;
  model: string | null;
  query_source: string | null;
  stop_reason: string | null;
  text: string | null;
  thinking_ms: number | null;
  rebuilt: boolean;
  reshaped: boolean;
  input_tokens: number | null;
  output_tokens: number | null;
  cache_read_tokens: number | null;
  cache_write_tokens: number | null;
  usd: number | null;
};

export type ToolNode = Timed & {
  kind: "tool";
  id: string;
  name: string;
  summary: string | null;
  input: string | null;
  agent_id: string | null;
  success: boolean | null;
  output: Output | null;
};

export type SubagentNode = Timed & {
  kind: "subagent";
  agent_id: string;
  agent_type: string | null;
  description: string | null;
  model: string | null;
  finished: boolean | null;
  last_activity: number | null;
  tokens: number;
  estimated_usd: number | null;
  tool_calls: number;
};

export type Node = RequestNode | ToolNode | SubagentNode;

export type Interaction = {
  id: string;
  prompt: string | null;
  started_at: number | null;
  ended_at: number | null;
  duration_ms: number | null;
  running: boolean;
  dropped: number;
  nodes: Node[];
};

export type Trace = { session: SessionSummary; interactions: Interaction[] };

export type Whole = {
  text: string;
  stderr: string | null;
  interrupted: boolean;
  other_blocks: number;
  truncated: boolean;
};

/** The server did not know this browser: the token was never exchanged. */
export class Unauthorized extends Error {}

/** The server has nothing at that address: a session that went away. */
export class NotFound extends Error {}

async function get<T>(path: string): Promise<T> {
  const response = await fetch(path, { credentials: "same-origin" });
  if (response.status === 401) throw new Unauthorized();
  if (response.status === 404) throw new NotFound();
  if (!response.ok) throw new Error(`${path}: ${response.status}`);
  return (await response.json()) as T;
}

// Ids go into the address as they are: the server takes plain names only.
const part = (id: string) => encodeURIComponent(id);

export const sessions = () =>
  get<{ sessions: SessionSummary[]; notice: string | null }>("/api/sessions");

export const detail = (session: string) => get<SessionDetail>(`/api/sessions/${part(session)}`);

export const trace = (session: string) => get<Trace>(`/api/sessions/${part(session)}/trace`);

export const wholeOutput = (session: string, call: string) =>
  get<Whole>(`/api/sessions/${part(session)}/calls/${part(call)}/output`);

export const wholeResponse = (session: string, message: string) =>
  get<Whole>(`/api/sessions/${part(session)}/responses/${part(message)}`);
