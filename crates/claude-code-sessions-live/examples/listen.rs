//! Listens for live events and prints one line per event: its time, session
//! and what happened. Names and numbers only; a tool call's summary is not
//! printed.
//!
//! ```text
//! cargo run -p claude-code-sessions-live --example listen [SOCKET_DIR]
//! ```
//!
//! With the bridge mod installed, start this and then use Claude Code.

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    use claude_code_sessions_live::event::Body;
    use claude_code_sessions_live::server::Server;

    let started = match std::env::args_os().nth(1) {
        Some(dir) => Server::start_in(std::path::Path::new(&dir)),
        None => Server::start(),
    };
    let (server, events) = match started {
        Ok(started) => started,
        Err(e) => {
            eprintln!("{e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    eprintln!("listening on {}", server.socket_path().display());
    for event in events {
        let what = match &event.body {
            Body::SessionStart(_) => "session.start".to_owned(),
            Body::SessionEnd(e) => format!("session.end {}", e.reason.as_deref().unwrap_or("")),
            Body::TurnStart(_) => "turn.start".to_owned(),
            Body::TurnComplete(e) => format!("turn.complete {:?}", e.reason),
            Body::ToolCall(e) => format!(
                "tool.call {:?} {}{}",
                e.phase,
                e.tool,
                e.success
                    .map_or(String::new(), |ok| format!(" success={ok}"))
            ),
            Body::ToolCheck(e) => format!("tool.check {} {:?}", e.tool, e.decision),
            Body::SessionMeasure(e) => format!(
                "session.measure context={:?} limits={}",
                e.context.map(|c| (c.tokens, c.window)),
                e.rate_limits.len()
            ),
            Body::AgentSpawn(e) => format!("agent.spawn {}", e.subagent_type),
            _ => "unknown".to_owned(),
        };
        let session = event.session_id.get(..8).unwrap_or(&event.session_id);
        println!("{} {session} {what}", event.ts);
    }
    std::process::ExitCode::SUCCESS
}

#[cfg(not(unix))]
fn main() {
    eprintln!("the socket server is unix only");
}
