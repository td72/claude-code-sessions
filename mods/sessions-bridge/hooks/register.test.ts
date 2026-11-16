// Drives the mod through one synthetic session and checks that every event
// it posts is, field for field, the protocol sample of the same name
// (tests/fixtures/events/, copied into ./samples.ts).

import { expect, test } from 'claude-code/testing'
import type { TestBody } from 'claude-code/testing'

import { samples } from './samples'

const SESSION = '22222222-2222-4222-8222-222222222222'
const AGENT = 'a1b2c3d4e5f60718'
const HOME = '/home/synthetic'
const SOCKETS = `${HOME}/.local/state/claude-code-sessions/sockets`

type Sent = { url: string; socketPath?: string; method?: string; event: Record<string, unknown> }

/** Stands in for the engine: answers the mod's calls and records its posts. */
function engine(on: Parameters<TestBody>[1], socketNames: string[] | 'missing') {
  const sent: Sent[] = []
  on('session.id', () => ({ value: SESSION }))
  on('session.model', () => ({ value: 'claude-fable-5-2' }))
  on('env.get', () => ({ value: HOME }))
  on('fs.list', () => {
    if (socketNames === 'missing') throw new Error('no such directory')
    return {
      value: socketNames.map(name => ({ name, kind: 'other' as const, size: 0, mtimeMs: 0, isLink: false })),
    }
  })
  on('http.fetch', (_, e) => {
    sent.push({
      url: e.url,
      socketPath: e.init?.socketPath,
      method: e.init?.method,
      event: JSON.parse(String(e.init?.body)) as Record<string, unknown>,
    })
    if (e.init?.socketPath?.endsWith('refuses.sock')) throw new Error('connection refused')
    return { value: { status: 200, ok: true, headers: {}, text: '' } }
  })
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('session.end', (_, e) => ({ sessionId: e.sessionId }))
  on('turn.start', (_, e) => ({ turnId: e.turnId }))
  on('turn.complete', () => ({ text: '' }))
  on('session.measure', (_, e) => ({ changed: e.changed }))
  return sent
}

/** The mod posts without waiting: give its posts a moment to land. */
async function settled(sent: Sent[], count: number): Promise<void> {
  for (let i = 0; i < 2000 && sent.length < count; i++) await Promise.resolve()
  for (let i = 0; i < 50; i++) await Promise.resolve()
}

/** What was sent as `type` (and `phase`, and by which thread). */
function find(sent: Sent[], type: string, where: Record<string, unknown> = {}) {
  return sent
    .map(s => s.event)
    .find(e => e.type === type && Object.entries(where).every(([k, v]) => e[k] === v))
}

/** A sample with the fields a test cannot fix taken from what was sent. */
function sample(name: string, actual: Record<string, unknown> | undefined, measured: string[] = []) {
  const expected: Record<string, unknown> = { ...samples[name] }
  expect(actual === undefined ? `${name}: nothing was sent` : name).toBe(name)
  for (const key of ['ts', ...measured]) {
    expect(typeof actual?.[key]).toBe('number')
    expected[key] = actual?.[key]
  }
  return expected
}

test('every event the mod sends is the protocol sample of its kind', async ($, on) => {
  const sent = engine(on, ['101.sock'])
  on('tool.check', (_, e) => ({ decision: e.tool === 'Write' ? 'ask' : 'allow' }))
  on('tool.call', () => ({ result: {} }))
  on('agent.spawn', () => ({ model: 'claude-haiku-5-5', agentId: AGENT }))

  await $.session.start({ cwd: '/work/other.repo', surface: 'terminal', isInteractive: true })
  await $.turn.start({ text: 'SYNTHETIC-TEXT prompt', turnId: 'turn-1' })
  await $.tool.check({ tool: 'Write', input: { file_path: '/work/x', content: 'SYNTHETIC-TEXT' }, tool_use_id: 'toolu_09' })
  // A call is checked before it runs: this raises `tool.check` too.
  await $.tool.call({ tool: 'Bash', command: 'cargo test --workspace', tool_use_id: 'toolu_01' })
  await $.agent.spawn({
    tool_use_id: 'toolu_02',
    prompt: 'SYNTHETIC-TEXT agent prompt',
    description: 'Synthetic agent task',
    subagentType: 'Explore',
    provider: { plugin: 'engine', tier: 'user' },
    parentModel: 'claude-fable-5-2',
    background: false,
    fork: false,
  })
  // Grep is a built-in of other builds than the one these types describe.
  const grep = { tool: 'Grep', pattern: 'synthetic pattern', tool_use_id: 'toolu_s1', agentId: AGENT }
  await $.tool.call(grep as unknown as Parameters<typeof $.tool.call>[0])
  await $.session.measure({
    context: { tokens: 6716, window: 200000 },
    rateLimits: [
      { kind: 'five_hour', percentUsed: 42.5, resetsAt: '2026-11-02T15:00:00.000Z' },
      { kind: 'seven_day', percentUsed: 7 },
    ],
    cost: { usd: 0.03 },
    changed: ['context'],
  })
  await $.turn.complete({ answer: 'SYNTHETIC-TEXT answer', durationMs: 8000, isAborted: false, turnId: 'turn-1', reason: 'answer' })
  await $.session.end({ reason: 'prompt_input_exit', sessionId: SESSION, resume: { id: SESSION } })
  await settled(sent, 13)

  expect(find(sent, 'session.start')).toEqual(sample('session-start', find(sent, 'session.start')))
  expect(find(sent, 'turn.start')).toEqual(sample('turn-start', find(sent, 'turn.start')))
  const allow = find(sent, 'tool.check', { decision: 'allow', tool: 'Bash' })
  expect(allow).toEqual(sample('tool-check-allow', allow))
  const ask = find(sent, 'tool.check', { decision: 'ask' })
  expect(ask).toEqual(sample('tool-check-ask', ask))
  const start = find(sent, 'tool.call', { phase: 'start', tool: 'Bash' })
  expect(start).toEqual(sample('tool-call-start', start))
  const end = find(sent, 'tool.call', { phase: 'end', tool: 'Bash' })
  expect(end).toEqual(sample('tool-call-end', end, ['duration_ms']))
  expect(find(sent, 'agent.spawn')).toEqual(sample('agent-spawn', find(sent, 'agent.spawn')))
  const sub = find(sent, 'tool.call', { phase: 'start', tool: 'Grep' })
  expect(sub).toEqual(sample('subagent-tool-call-start', sub))
  expect(find(sent, 'session.measure')).toEqual(sample('session-measure', find(sent, 'session.measure')))
  expect(find(sent, 'turn.complete')).toEqual(sample('turn-complete', find(sent, 'turn.complete')))
  expect(find(sent, 'session.end')).toEqual(sample('session-end', find(sent, 'session.end')))

  // The 11 kinds above, plus the check and the end of the subagent's call.
  const kinds = sent.map(s => [s.event.type, s.event.phase, s.event.decision].filter(Boolean).join(' ')).sort()
  expect(kinds).toEqual([
    'agent.spawn',
    'session.end',
    'session.measure',
    'session.start',
    'tool.call end',
    'tool.call end',
    'tool.call start',
    'tool.call start',
    'tool.check allow',
    'tool.check allow',
    'tool.check ask',
    'turn.complete',
    'turn.start',
  ])
  for (const post of sent) {
    expect(post.url).toBe('http://claude-code-sessions/events')
    expect(post.method).toBe('POST')
    expect(post.socketPath).toBe(`${SOCKETS}/101.sock`)
    expect(post.event.v).toBe(1)
    expect(post.event.session_id).toBe(SESSION)
  }
  // No prompt, answer, file content or agent prompt in anything sent.
  expect(JSON.stringify(sent).includes('SYNTHETIC-TEXT')).toBe(false)
})

test('each event goes to every socket, and a socket that refuses is ignored', async ($, on) => {
  const sent = engine(on, ['101.sock', 'refuses.sock', '202.sock', 'notes.txt'])
  on('tool.call', () => ({ result: { stdout: 'kept' } }))

  const ran = await $.tool.call({ tool: 'Bash', command: 'ls', tool_use_id: 'toolu_01' })
  await settled(sent, 6)

  // The call's own result is untouched.
  expect(ran.result).toEqual({ stdout: 'kept' })
  const targets = (phase: string) =>
    sent.filter(s => s.event.phase === phase).map(s => s.socketPath).sort()
  const all = ['101.sock', '202.sock', 'refuses.sock'].map(name => `${SOCKETS}/${name}`)
  expect(targets('start')).toEqual(all)
  expect(targets('end')).toEqual(all)
})

test('with no consumer the mod stays out of the way', async ($, on) => {
  const sent = engine(on, 'missing')
  on('tool.call', () => ({ isError: true, result: 'boom' }))

  await $.session.start({ cwd: '/work/example', surface: 'terminal', isInteractive: false })
  const ran = await $.tool.call({ tool: 'Bash', command: 'false', tool_use_id: 'toolu_01' })
  await $.session.end({ reason: 'other', sessionId: SESSION, resume: { id: SESSION } })
  await settled(sent, 0)

  expect(ran.isError).toBe(true)
  expect(sent.length).toBe(0)
})

test('a failed or denied tool call is reported as not successful', async ($, on) => {
  const sent = engine(on, ['101.sock'])
  on('tool.call', (_, e) => (e.tool === 'Bash' ? { isError: true, result: 'boom' } : { deny: 'not here' }))

  await $.tool.call({ tool: 'Bash', command: 'false', tool_use_id: 'toolu_01' })
  await $.tool.call({ tool: 'Read', file_path: '/work/example/README.md', tool_use_id: 'toolu_02' })
  await settled(sent, 4)

  expect(find(sent, 'tool.call', { phase: 'end', tool: 'Bash' })?.success).toBe(false)
  expect(find(sent, 'tool.call', { phase: 'end', tool: 'Read' })?.success).toBe(false)
  // A long or multi-line input is cut to one short line.
  expect(find(sent, 'tool.call', { phase: 'start', tool: 'Read' })?.summary).toBe('/work/example/README.md')
})
