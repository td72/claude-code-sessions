//! The socket server, end to end over a real Unix socket.
#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use iktinos_live::event::Body;
use iktinos_live::server::Server;

/// A fresh directory with a short path: a socket path has little room.
fn scratch() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ccs-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn sample(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/events")
        .join(format!("{name}.json"));
    fs::read_to_string(path).unwrap()
}

/// Sends raw bytes and returns the status line of the answer.
fn send(socket: &Path, request: &[u8]) -> String {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(request).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response.lines().next().unwrap_or_default().to_owned()
}

fn post(socket: &Path, body: &str) -> String {
    let request = format!(
        "POST /events HTTP/1.1\r\nHost: iktinos\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    send(socket, request.as_bytes())
}

#[test]
fn a_posted_event_arrives_on_the_channel() {
    let dir = scratch();
    let (server, events) = Server::start_in(&dir).unwrap();
    let socket = server.socket_path().to_owned();
    assert_eq!(
        socket,
        dir.join(format!("{}.sock", std::process::id())),
        "one socket per process, named after its pid"
    );

    assert_eq!(post(&socket, &sample("tool-call-start")), "HTTP/1.1 200 OK");
    let event = events.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(event.session_id, "22222222-2222-4222-8222-222222222222");
    assert!(matches!(event.body, Body::ToolCall(call) if call.tool == "Bash"));

    // One request per connection, any number of connections, in order.
    for name in ["tool-call-end", "session-measure", "unknown-type"] {
        assert_eq!(post(&socket, &sample(name)), "HTTP/1.1 200 OK", "{name}");
    }
    let bodies: Vec<Body> = (0..3)
        .map(|_| events.recv_timeout(Duration::from_secs(10)).unwrap().body)
        .collect();
    assert!(matches!(bodies[0], Body::ToolCall(_)));
    assert!(matches!(bodies[1], Body::SessionMeasure(_)));
    // A type the protocol does not know is still delivered, for the merge
    // to ignore.
    assert_eq!(bodies[2], Body::Unknown);

    drop(server);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_malformed_request_is_answered_and_ignored() {
    let dir = scratch();
    let (server, events) = Server::start_in(&dir).unwrap();
    let socket = server.socket_path();

    assert_eq!(send(socket, b"garbage\r\n\r\n"), "HTTP/1.1 400 Bad Request");
    assert_eq!(
        send(socket, b"GET /events HTTP/1.1\r\nContent-Length: 0\r\n\r\n"),
        "HTTP/1.1 405 Method Not Allowed"
    );
    assert_eq!(
        post(socket, "{\"not\":\"an event\"}"),
        "HTTP/1.1 400 Bad Request"
    );
    // A client that connects and leaves.
    drop(UnixStream::connect(socket).unwrap());
    assert!(events.try_recv().is_err());

    // The server is still there for the next one.
    assert_eq!(post(socket, &sample("turn-start")), "HTTP/1.1 200 OK");
    let event = events.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(matches!(event.body, Body::TurnStart(_)));
    assert!(events.try_recv().is_err());
    // The three that were turned away are counted; the client that said
    // nothing is not.
    assert_eq!(server.rejected(), 3);

    drop(server);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn stale_sockets_are_removed_and_live_ones_kept() {
    let dir = scratch();
    fs::create_dir_all(&dir).unwrap();
    // A consumer that was killed: its socket file outlives its listener.
    let stale = dir.join("1.sock");
    drop(UnixListener::bind(&stale).unwrap());
    assert!(stale.exists());
    // A consumer that is alive.
    let live = dir.join("2.sock");
    let _other = UnixListener::bind(&live).unwrap();
    // Not a socket of ours.
    let unrelated = dir.join("notes.txt");
    fs::write(&unrelated, "kept").unwrap();

    let (server, _events) = Server::start_in(&dir).unwrap();
    assert!(!stale.exists());
    assert!(live.exists());
    assert!(unrelated.exists());
    assert!(server.socket_path().exists());

    drop(server);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn the_socket_is_removed_on_drop_and_the_directory_is_private() {
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch();
    let (server, _events) = Server::start_in(&dir).unwrap();
    let socket = server.socket_path().to_owned();
    assert!(socket.exists());
    assert_eq!(
        fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );

    drop(server);
    assert!(!socket.exists());
    assert!(UnixStream::connect(&socket).is_err());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_second_server_in_the_same_process_cannot_take_the_socket() {
    let dir = scratch();
    let (server, _events) = Server::start_in(&dir).unwrap();
    assert!(Server::start_in(&dir).is_err());
    // The failed start did not take the first one's socket away.
    assert_eq!(
        post(server.socket_path(), &sample("turn-start")),
        "HTTP/1.1 200 OK"
    );
    drop(server);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn the_server_stops_when_nobody_receives() {
    let dir = scratch();
    let (server, events) = Server::start_in(&dir).unwrap();
    drop(events);
    // The event cannot be delivered; the listener gives up quietly, and
    // dropping the server still returns.
    let _ = UnixStream::connect(server.socket_path()).map(|mut stream| {
        let body = sample("turn-start");
        let _ = write!(
            stream,
            "POST /events HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
    });
    drop(server);
    let _ = fs::remove_dir_all(dir);
}
