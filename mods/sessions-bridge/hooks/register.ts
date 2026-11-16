// sessions-bridge: reports what a session is doing to every
// local consumer of claude-code-sessions (vig, the standalone viewer).
//
// Each consumer listens on its own Unix socket under
// ~/.local/state/claude-code-sessions/sockets/. On every event the mod posts
// one small JSON object (protocol version 1, see the
// claude-code-sessions-live crate and tests/fixtures/events/) to each of
// them. The mod only observes: every hook hands the event on unchanged,
// never waits for a consumer, and ignores a socket that refuses.
//
// What is sent: names, ids, numbers and a one-line summary of a tool call
// (the command, the file path). Never a prompt, a response, a tool's output
// or the environment, and nothing leaves the machine.

import type { EngineInterface, Register } from 'claude-code'

export const VERSION = 1
export const SOCKET_DIR = '.local/state/claude-code-sessions/sockets'
const URL = 'http://claude-code-sessions/events'

/** How long a listing of the socket directory is reused. */
const SOCKETS_FRESH_MS = 2000
/** How long the mod waits for consumers when the session is ending. */
const FLUSH_MS = 300
const SUMMARY_CHARS = 120

type Body = { type: string } & Record<string, unknown>

let sessionId: string | undefined
let sockets: { paths: string[]; listedAt: number } | undefined

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const started = await next(e)
    // A reload runs this again: the module's variables start over with it.
    const model = await $.session.model().catch(() => undefined)
    send($, {
      type: 'session.start',
      cwd: started.cwd,
      model,
      interactive: e.isInteractive,
    })
    return started
  })

  on('session.end', async ($, e, next) => {
    // The process is about to exit: this one event is waited for, briefly.
    await Promise.race([
      post($, { type: 'session.end', reason: e.reason }, Date.now()),
      $.clock.sleep(FLUSH_MS),
    ]).catch(() => undefined)
    return next(e)
  })

  on('turn.start', ($, e, next) => {
    send($, { type: 'turn.start', turn_id: e.turnId, agent_id: e.agentId })
    return next(e)
  })

  on('turn.complete', ($, e, next) => {
    send($, {
      type: 'turn.complete',
      turn_id: e.turnId,
      agent_id: e.agentId,
      reason: e.reason,
      duration_ms: e.durationMs,
    })
    return next(e)
  })

  on('tool.check', async ($, e, next) => {
    const checked = await next(e)
    send($, {
      type: 'tool.check',
      tool: e.tool,
      tool_use_id: e.tool_use_id,
      agent_id: e.agentId,
      decision: checked.decision,
    })
    return checked
  })

  on('tool.call', async ($, e, next) => {
    const tool = String(e.tool)
    const call = { tool, tool_use_id: e.tool_use_id, agent_id: e.agentId }
    const startedAt = Date.now()
    send($, { type: 'tool.call', phase: 'start', ...call, summary: summarize(e) })
    let hasSucceeded = false
    try {
      const ran = await next(e)
      hasSucceeded = ran.deny === undefined && ran.isError !== true
      return ran
    } finally {
      send($, {
        type: 'tool.call',
        phase: 'end',
        ...call,
        duration_ms: Date.now() - startedAt,
        success: hasSucceeded,
      })
    }
  })

  on('agent.spawn', async ($, e, next) => {
    const spawned = await next(e)
    if (spawned.deny === undefined) {
      send($, {
        type: 'agent.spawn',
        tool_use_id: e.tool_use_id,
        agent_id: spawned.agentId,
        subagent_type: e.subagentType,
        description: line(e.description),
        model: spawned.model,
        background: e.background,
      })
    }
    return spawned
  })

  on('session.measure', ($, e, next) => {
    send($, {
      type: 'session.measure',
      context: { tokens: e.context.tokens, window: e.context.window },
      rate_limits: e.rateLimits.map(limit => ({
        kind: limit.kind,
        percent_used: limit.percentUsed,
        resets_at: limit.resetsAt,
      })),
      cost_usd: e.cost?.usd,
    })
    return next(e)
  })
}

/** Posts an event without making the session wait for it. */
function send($: EngineInterface, body: Body): void {
  // The time is taken now; the delivery may finish later and out of order,
  // and the receiving side orders by `ts`.
  void post($, body, Date.now()).catch(() => undefined)
}

async function post($: EngineInterface, body: Body, ts: number): Promise<void> {
  sessionId ??= await $.session.id()
  // JSON drops the fields that are undefined.
  const event = JSON.stringify({ v: VERSION, session_id: sessionId, ts, ...body })
  const paths = await listSockets($)
  await Promise.all(
    paths.map(socketPath =>
      $.http
        .fetch(URL, {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: event,
          socketPath,
        })
        // A consumer that went away is not this session's problem.
        .catch(() => undefined),
    ),
  )
}

/** The sockets consumers listen on right now; none when nobody does. */
async function listSockets($: EngineInterface): Promise<string[]> {
  const now = Date.now()
  if (sockets !== undefined && now - sockets.listedAt < SOCKETS_FRESH_MS) {
    return sockets.paths
  }
  let paths: string[] = []
  try {
    const home = await $.env.get('HOME')
    if (home !== undefined && home !== '') {
      const dir = `${home}/${SOCKET_DIR}`
      const entries = await $.fs.list(dir)
      paths = entries
        .filter(entry => entry.name.endsWith('.sock'))
        .map(entry => `${dir}/${entry.name}`)
    }
  } catch {
    // No directory: no consumer has ever run.
  }
  sockets = { paths, listedAt: now }
  return paths
}

/** One line saying what a tool call is about, when its input has one. */
export function summarize(input: Record<string, unknown>): string | undefined {
  for (const key of ['command', 'file_path', 'notebook_path', 'pattern', 'url', 'query', 'description']) {
    const value = input[key]
    if (typeof value === 'string' && value.trim() !== '') return line(value)
  }
  return undefined
}

/** The first line of `text`, cut to a length worth sending. */
function line(text: string): string {
  const first = text.trim().split('\n', 1)[0] ?? ''
  return first.length > SUMMARY_CHARS ? `${first.slice(0, SUMMARY_CHARS - 1)}…` : first
}
