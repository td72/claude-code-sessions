//! The server, asked over a real socket: who is answered, and with what.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use iktinos_core::record::KNOWN_CLAUDE_CODE_VERSION;
use iktinos_web::{Config, Server};
use serde_json::Value;

const INDEXED: &str = "11111111-1111-4111-8111-111111111111";
const WITH_SUBAGENTS: &str = "22222222-2222-4222-8222-222222222222";
const UNINDEXED: &str = "33333333-3333-4333-8333-333333333333";
const MARKER: &str = "SYNTHETIC-TEXT";

fn fixture_projects() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(KNOWN_CLAUDE_CODE_VERSION)
        .join("projects")
}

/// A copy of the fixture projects that a test may write to.
fn scratch(name: &str) -> PathBuf {
    fn copy(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.path().is_dir() {
                copy(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    let dir = std::env::temp_dir().join(format!("iktinos-web-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    copy(&fixture_projects(), &dir);
    dir
}

/// A server on a free port over `projects`, without live events.
fn server_over(projects: PathBuf) -> Server {
    Server::start(Config {
        port: 0,
        live: false,
        refresh: Duration::from_millis(50),
        ..Config::new(projects)
    })
    .unwrap()
}

fn server() -> Server {
    server_over(fixture_projects())
}

struct Answer {
    status: u16,
    head: String,
    body: String,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.head
            .lines()
            .filter_map(|line| line.split_once(": "))
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }
}

/// Sends a request as it is written and reads the whole answer.
fn send(addr: SocketAddr, request: &str) -> Answer {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut text = String::new();
    stream.read_to_string(&mut text).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status: {text}"));
    Answer {
        status,
        head: head.to_owned(),
        body: body.to_owned(),
    }
}

/// `GET path` as this server's own page would ask it: its address as the
/// host, and the cookie.
fn get(server: &Server, path: &str) -> Answer {
    send(
        server.addr(),
        &format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\nCookie: iktinos={}\r\n\r\n",
            server.addr(),
            server.token()
        ),
    )
}

#[test]
fn it_listens_on_this_machine_only_and_says_where() {
    let server = server();
    assert!(server.addr().ip().is_loopback());
    assert_ne!(server.addr().port(), 0, "a free port was picked");
    assert_eq!(
        server.url(),
        format!("http://{}/?token={}", server.addr(), server.token())
    );
    assert_eq!(server.notice(), None);
}

#[test]
fn nothing_is_answered_without_the_token_or_its_cookie() {
    let server = server();
    let host = server.addr();
    let ask = |rest: &str| send(host, &format!("GET {rest}"));

    // No token, no cookie: every path says the same and gives nothing.
    for path in ["/", "/api/sessions", "/api/events", "/no-such-thing"] {
        let answer = ask(&format!("{path} HTTP/1.1\r\nHost: {host}\r\n\r\n"));
        assert_eq!(answer.status, 401, "{path}");
        assert!(
            !answer.body.contains("Synthetic"),
            "{path}: {}",
            answer.body
        );
    }
    // A wrong token, a wrong cookie, a token that only starts right.
    let wrong = ask(&format!("/?token=0000 HTTP/1.1\r\nHost: {host}\r\n\r\n"));
    assert_eq!(wrong.status, 401);
    assert_eq!(wrong.header("set-cookie"), None);
    let short = &server.token()[..63];
    let cookie = ask(&format!(
        "/api/sessions HTTP/1.1\r\nHost: {host}\r\nCookie: iktinos={short}\r\n\r\n"
    ));
    assert_eq!(cookie.status, 401);

    // The token: a cookie no script can read and no other site can send,
    // and a redirect to the same place without the token.
    let first = ask(&format!(
        "/?token={} HTTP/1.1\r\nHost: {host}\r\n\r\n",
        server.token()
    ));
    assert_eq!(first.status, 303);
    assert_eq!(first.header("location"), Some("/"));
    let set = first.header("set-cookie").unwrap();
    assert_eq!(
        set,
        format!(
            "iktinos={}; Path=/; HttpOnly; SameSite=Strict",
            server.token()
        )
    );
    assert!(first.body.is_empty());

    // With the cookie: the page.
    let page = get(&server, "/");
    assert_eq!(page.status, 200);
    assert_eq!(
        page.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert!(page.body.contains("<title>iktinos</title>"));
    // No inline script or style: the policy would not run them. A script
    // is a file this server serves, and it is served.
    assert!(!page.body.contains("<style"));
    for script in page.body.split("<script").skip(1) {
        let tag = script.split('>').next().unwrap();
        let src = tag.split("src=\"").nth(1).expect("an inline script");
        let address = src.split('"').next().unwrap();
        assert!(address.starts_with('/'), "{address}");
        let served = get(&server, address);
        assert_eq!(served.status, 200, "{address}");
        assert_eq!(
            served.header("content-type"),
            Some("text/javascript; charset=utf-8")
        );
    }
}

#[test]
fn only_its_own_address_and_only_get_are_answered() {
    let server = server();
    let (host, token) = (server.addr(), server.token());
    let with_host = |name: &str| {
        send(
            host,
            &format!(
                "GET /api/sessions HTTP/1.1\r\nHost: {name}\r\nCookie: iktinos={token}\r\n\r\n"
            ),
        )
    };
    // A name that resolves to this machine is not this server's address.
    for other in [
        "elsewhere.example".to_owned(),
        format!("elsewhere.example:{}", host.port()),
        "127.0.0.1".to_owned(),
        format!("127.0.0.1:{}", host.port().wrapping_add(1)),
        String::new(),
    ] {
        assert_eq!(with_host(&other).status, 403, "{other}");
    }
    assert_eq!(with_host(&format!("localhost:{}", host.port())).status, 200);
    assert_eq!(with_host(&host.to_string()).status, 200);
    // No host at all.
    let none = send(
        host,
        &format!("GET / HTTP/1.1\r\nCookie: iktinos={token}\r\n\r\n"),
    );
    assert_eq!(none.status, 403);

    // It only reads.
    for method in ["POST", "PUT", "DELETE", "PATCH", "OPTIONS", "HEAD"] {
        let answer = send(
            host,
            &format!(
                "{method} /api/sessions HTTP/1.1\r\nHost: {host}\r\nCookie: iktinos={token}\r\nContent-Length: 0\r\n\r\n"
            ),
        );
        assert_eq!(answer.status, 405, "{method}");
        assert_eq!(answer.header("allow"), Some("GET"), "{method}");
    }

    // What is not a request, and a client that says nothing.
    assert_eq!(send(host, "garbage\r\n\r\n").status, 400);
    drop(TcpStream::connect(host).unwrap());
    assert_eq!(get(&server, "/api/sessions").status, 200, "still there");
}

#[test]
fn the_sessions_are_listed_the_latest_first() {
    let server = server();
    let answer = get(&server, "/api/sessions");
    assert_eq!(answer.status, 200);
    assert_eq!(answer.header("content-type"), Some("application/json"));
    // Every answer confines the page and is not kept.
    assert_eq!(answer.header("cache-control"), Some("no-store"));
    assert_eq!(answer.header("x-content-type-options"), Some("nosniff"));
    let policy = answer.header("content-security-policy").unwrap();
    assert!(policy.contains("script-src 'self'") && policy.contains("connect-src 'self'"));

    let json = answer.json();
    assert_eq!(json["notice"], Value::Null);
    let sessions = json["sessions"].as_array().unwrap();
    let ids: Vec<&str> = sessions.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [UNINDEXED, WITH_SUBAGENTS, INDEXED]);

    let titled = &sessions[2];
    assert_eq!(titled["label"], "Synthetic session title");
    assert_eq!(titled["location"], "/work/example");
    assert_eq!(titled["location_name"], "example");
    assert_eq!(titled["branch"], "main");
    assert_eq!(titled["status"]["kind"], "idle");
    assert!(titled["tokens"].as_u64().unwrap() > 0);
    assert_eq!(titled["cost"]["source"], "estimated");
    assert!(titled["cost"]["usd"].as_f64().unwrap() > 0.0);
    assert!(titled["last_seen"].as_i64().unwrap() > 0);

    let other = &sessions[1];
    assert_eq!(other["label"], "22222222");
    assert_eq!(
        (&other["subagents"], &other["interactions"]),
        (&2.into(), &1.into())
    );
}

#[test]
fn a_session_is_served_with_its_detail_and_its_trace() {
    let server = server();
    let detail = get(&server, &format!("/api/sessions/{WITH_SUBAGENTS}")).json();
    assert_eq!(detail["id"], WITH_SUBAGENTS);
    assert_eq!(detail["location_name"], "other.repo");
    assert!(detail["context"]["tokens"].as_u64().unwrap() > 0);
    let models: Vec<&str> = detail["by_model"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(models, ["claude-fable-5-2", "claude-haiku-5-5"]);
    assert!(!detail["by_source"].as_array().unwrap().is_empty());
    assert_eq!(detail["subagents"].as_array().unwrap().len(), 2);
    // The latest call first, with the start of what it returned.
    let tools = detail["recent_tools"].as_array().unwrap();
    assert_eq!(tools.len(), 5);
    let bash = tools.iter().find(|t| t["id"] == "toolu_01").unwrap();
    assert_eq!(bash["name"], "Bash");
    assert_eq!(bash["summary"], "cargo test --workspace");
    assert_eq!(bash["success"], true);
    assert_eq!(bash["output"]["text"], "Synthetic output of toolu_01");

    let trace = get(&server, &format!("/api/sessions/{WITH_SUBAGENTS}/trace")).json();
    assert_eq!(trace["session"]["id"], WITH_SUBAGENTS);
    let interactions = trace["interactions"].as_array().unwrap();
    assert_eq!(interactions.len(), 1);
    let interaction = &interactions[0];
    assert_eq!(interaction["id"], "prompt-u1");
    assert_eq!(interaction["prompt"], "Synthetic prompt: run the checks");
    assert_eq!(interaction["running"], false);
    let shape: Vec<String> = interaction["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| {
            let name = match node["kind"].as_str().unwrap() {
                "tool" => node["name"].as_str().unwrap(),
                kind => kind,
            };
            format!(
                "{}{name}",
                "  ".repeat(node["depth"].as_u64().unwrap() as usize)
            )
        })
        .collect();
    // The calls under the response that made them, a subagent under the
    // call that spawned it and what it did under it.
    assert_eq!(
        shape,
        [
            "llm_request",
            "  Bash",
            "  Read",
            "    subagent",
            "      llm_request",
            "        Grep",
            "      llm_request",
            "llm_request",
            "  mcp__example__lookup",
            "llm_request",
            "subagent",
            "  llm_request",
            "    Grep",
            "  llm_request",
        ]
    );
    let said = interaction["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|node| node["text"].as_str())
        .next_back()
        .unwrap();
    assert_eq!(said, "Synthetic answer: the checks pass");

    // Nothing that is not modelled is in what the lists and the trees say.
    for path in [
        "/api/sessions".to_owned(),
        format!("/api/sessions/{WITH_SUBAGENTS}"),
        format!("/api/sessions/{WITH_SUBAGENTS}/trace"),
        format!("/api/sessions/{INDEXED}"),
        format!("/api/sessions/{INDEXED}/trace"),
    ] {
        assert!(!get(&server, &path).body.contains(MARKER), "{path}");
    }
}

#[test]
fn what_is_read_again_is_named_by_ids_never_by_a_path() {
    let server = server();
    let session = format!("/api/sessions/{WITH_SUBAGENTS}");

    // The whole output of a call: here the one moved to a file of its own.
    let output = get(&server, &format!("{session}/calls/toolu_01/output")).json();
    assert!(
        output["text"]
            .as_str()
            .unwrap()
            .starts_with("Synthetic whole output of toolu_01"),
        "{output}"
    );
    // The whole text of a response, past its first line.
    let said = get(&server, &format!("{session}/responses/msg_3")).json();
    assert_eq!(
        said["text"],
        "Synthetic answer: the checks pass\nSYNTHETIC-TEXT invented for the fixture"
    );

    // What is not there.
    for (path, status) in [
        (format!("{session}/calls/toolu_none/output"), 404),
        (format!("{session}/responses/msg_none"), 404),
        (
            "/api/sessions/00000000-0000-4000-8000-000000000000".to_owned(),
            404,
        ),
        (
            "/api/sessions/00000000-0000-4000-8000-000000000000/calls/toolu_01/output".to_owned(),
            404,
        ),
        // Another session's call.
        (
            format!("/api/sessions/{INDEXED}/calls/toolu_01/output"),
            404,
        ),
        ("/api/usage".to_owned(), 404),
        ("/api".to_owned(), 404),
        ("/assets/../Cargo.toml".to_owned(), 404),
    ] {
        assert_eq!(get(&server, &path).status, status, "{path}");
    }
    // Nothing in an address is taken as part of a file's path.
    for id in [
        "..",
        "%2e%2e",
        "..%2f..%2fetc",
        "a.b",
        "toolu_01%00",
        "toolu_01.txt",
    ] {
        for path in [
            format!("/api/sessions/{id}"),
            format!("/api/sessions/{id}/trace"),
            format!("{session}/calls/{id}/output"),
            format!("{session}/responses/{id}"),
        ] {
            let answer = get(&server, &path);
            assert_eq!(answer.status, 404, "{path}");
            assert!(!answer.body.contains("Synthetic"), "{path}");
        }
    }
}

#[test]
fn a_server_narrowed_to_a_directory_serves_its_sessions_only() {
    let server = Server::start(Config {
        port: 0,
        live: false,
        within: Some("/work/example".into()),
        ..Config::new(fixture_projects())
    })
    .unwrap();
    let listed = get(&server, "/api/sessions").json();
    let ids: Vec<&str> = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [UNINDEXED, INDEXED]);
    assert_eq!(
        get(&server, &format!("/api/sessions/{INDEXED}")).status,
        200
    );
    // The other session is not served under any of its addresses.
    for rest in ["", "/trace", "/calls/toolu_01/output", "/responses/msg_3"] {
        let path = format!("/api/sessions/{WITH_SUBAGENTS}{rest}");
        assert_eq!(get(&server, &path).status, 404, "{path}");
    }
}

#[test]
fn a_change_is_announced_on_the_event_stream() {
    let projects = scratch("events");
    let server = server_over(projects.clone());
    let mut stream = TcpStream::connect(server.addr()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(
        stream,
        "GET /api/events HTTP/1.1\r\nHost: {}\r\nCookie: iktinos={}\r\n\r\n",
        server.addr(),
        server.token()
    )
    .unwrap();
    // Reads until `text` has been seen, a piece at a time.
    let mut seen = String::new();
    let mut until = |text: &str| {
        let mut chunk = [0u8; 1024];
        while !seen.contains(text) {
            let n = stream.read(&mut chunk).unwrap();
            assert!(n > 0, "the stream ended: {seen}");
            seen.push_str(&String::from_utf8_lossy(&chunk[..n]));
        }
        seen.clone()
    };

    // The stream opens with where things stand.
    let opened = until("data: 0\n\n");
    assert!(opened.starts_with("HTTP/1.1 200 OK\r\n"), "{opened}");
    assert!(opened.contains("Content-Type: text/event-stream\r\n"));
    assert!(opened.contains("event: changed\ndata: 0\n\n"));

    // One more record in a transcript: the next event, and the list has it.
    let before = get(&server, &format!("/api/sessions/{WITH_SUBAGENTS}")).json();
    let transcript = projects
        .join("-work-other-repo")
        .join(format!("{WITH_SUBAGENTS}.jsonl"));
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(transcript)
        .unwrap();
    writeln!(
        file,
        r#"{{"parentUuid":null,"isSidechain":false,"promptId":"prompt-e1","type":"user","message":{{"role":"user","content":"Synthetic prompt: one more"}},"uuid":"e1","timestamp":"2026-11-02T11:05:00.000Z","cwd":"/work/other.repo","sessionId":"{WITH_SUBAGENTS}","version":"2.1.325","gitBranch":"main"}}"#
    )
    .unwrap();
    until("event: changed\ndata: 1\n\n");
    let after = get(&server, &format!("/api/sessions/{WITH_SUBAGENTS}")).json();
    assert_eq!(
        after["interactions"].as_u64().unwrap(),
        before["interactions"].as_u64().unwrap() + 1
    );

    // Stopping the server ends the stream.
    drop(server);
    let mut rest = Vec::new();
    let _ = stream.read_to_end(&mut rest);
    let _ = fs::remove_dir_all(projects);
}

#[test]
fn a_projects_directory_that_is_missing_is_said_and_served_as_empty() {
    let server = server_over("/no-such-root/projects".into());
    assert!(server.notice().is_some());
    let listed = get(&server, "/api/sessions").json();
    assert_eq!(listed["sessions"].as_array().unwrap().len(), 0);
    assert!(listed["notice"]
        .as_str()
        .unwrap()
        .contains("/no-such-root/projects"));
}
