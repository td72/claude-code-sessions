// Generated from tests/fixtures/events/*.json. Do not edit: run
//   UPDATE_SAMPLES=1 cargo test -p iktinos-live --test mod_samples
// The samples are the protocol's contract; register.test.ts checks that the
// mod sends exactly these shapes.
export const samples: Record<string, Record<string, unknown>> = {
  "agent-spawn": {
    "agent_id": "a1b2c3d4e5f60718",
    "background": false,
    "description": "Synthetic agent task",
    "model": "claude-haiku-5-5",
    "session_id": "22222222-2222-4222-8222-222222222222",
    "subagent_type": "Explore",
    "tool_use_id": "toolu_02",
    "ts": 1793613602500,
    "type": "agent.spawn",
    "v": 1
  },
  "session-end": {
    "reason": "prompt_input_exit",
    "session_id": "22222222-2222-4222-8222-222222222222",
    "ts": 1793613609000,
    "type": "session.end",
    "v": 1
  },
  "session-measure": {
    "context": {
      "tokens": 6716,
      "window": 200000
    },
    "cost_usd": 0.03,
    "rate_limits": [
      {
        "kind": "five_hour",
        "percent_used": 42.5,
        "resets_at": "2026-11-02T15:00:00.000Z"
      },
      {
        "kind": "seven_day",
        "percent_used": 7.0
      }
    ],
    "session_id": "22222222-2222-4222-8222-222222222222",
    "ts": 1793613608050,
    "type": "session.measure",
    "v": 1
  },
  "session-start": {
    "cwd": "/work/other.repo",
    "interactive": true,
    "model": "claude-fable-5-2",
    "session_id": "22222222-2222-4222-8222-222222222222",
    "ts": 1793613600000,
    "type": "session.start",
    "v": 1
  },
  "subagent-tool-call-start": {
    "agent_id": "a1b2c3d4e5f60718",
    "phase": "start",
    "session_id": "22222222-2222-4222-8222-222222222222",
    "summary": "synthetic pattern",
    "tool": "Grep",
    "tool_use_id": "toolu_s1",
    "ts": 1793613605000,
    "type": "tool.call",
    "v": 1
  },
  "tool-call-end": {
    "duration_ms": 1500,
    "phase": "end",
    "session_id": "22222222-2222-4222-8222-222222222222",
    "success": true,
    "tool": "Bash",
    "tool_use_id": "toolu_01",
    "ts": 1793613603500,
    "type": "tool.call",
    "v": 1
  },
  "tool-call-start": {
    "phase": "start",
    "session_id": "22222222-2222-4222-8222-222222222222",
    "summary": "cargo test --workspace",
    "tool": "Bash",
    "tool_use_id": "toolu_01",
    "ts": 1793613602000,
    "type": "tool.call",
    "v": 1
  },
  "tool-check-allow": {
    "decision": "allow",
    "session_id": "22222222-2222-4222-8222-222222222222",
    "tool": "Bash",
    "tool_use_id": "toolu_01",
    "ts": 1793613601900,
    "type": "tool.check",
    "v": 1
  },
  "tool-check-ask": {
    "decision": "ask",
    "session_id": "22222222-2222-4222-8222-222222222222",
    "tool": "Write",
    "tool_use_id": "toolu_09",
    "ts": 1793613601950,
    "type": "tool.check",
    "v": 1
  },
  "turn-complete": {
    "duration_ms": 8000,
    "reason": "answer",
    "session_id": "22222222-2222-4222-8222-222222222222",
    "ts": 1793613608100,
    "turn_id": "turn-1",
    "type": "turn.complete",
    "v": 1
  },
  "turn-start": {
    "session_id": "22222222-2222-4222-8222-222222222222",
    "ts": 1793613600100,
    "turn_id": "turn-1",
    "type": "turn.start",
    "v": 1
  },
  "unknown-type": {
    "frames": 3,
    "session_id": "22222222-2222-4222-8222-222222222222",
    "ts": 1793613609500,
    "type": "hologram.frame",
    "v": 1
  }
}
