//! The receiving end: a Unix socket that accepts `POST /events`.
//!
//! Every consumer (vig, the standalone viewer) listens on its own socket,
//! `<socket dir>/<pid>.sock`, and the bridge mod posts each event to every
//! socket it finds in the directory. [`Server::start`] uses the directory all
//! consumers share, [`default_socket_dir`]; a test passes its own.
//!
//! The server speaks just enough HTTP/1.1 for that: one request per
//! connection, a request line, headers, a `Content-Length` body of at most
//! [`MAX_BODY_BYTES`]. A well-formed event is sent through the channel and
//! answered `200`; anything else is answered with an error status and
//! dropped. Nothing is ever sent to Claude Code beyond that status line.
//!
//! The server counts the requests it turned away ([`Server::rejected`]): a
//! consumer that hears nothing can then tell a mod that does not send from
//! one that sends what this crate does not read. A connection that sends
//! nothing at all (another consumer checking whether the socket is alive)
//! is not a request and is not counted.
//!
//! On start, sockets in the directory that refuse a connection (left behind
//! by a consumer that was killed) are removed. On drop the server removes its
//! own.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::event::Event;

/// The largest event body accepted.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// The largest request head (request line and headers) accepted.
const MAX_HEAD_BYTES: usize = 8 * 1024;

/// How long a client may take to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// The directory every consumer listens in and the mod posts to:
/// `~/.local/state/iktinos/sockets`. `None` without a home
/// directory.
pub fn default_socket_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join(".local/state/iktinos/sockets"))
}

/// A listening socket. Dropping it stops the listener and removes the socket.
#[derive(Debug)]
pub struct Server {
    path: PathBuf,
    stopping: Arc<AtomicBool>,
    rejected: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Listens in [`default_socket_dir`]. Events arrive on the receiver.
    pub fn start() -> Result<(Self, Receiver<Event>), ServerError> {
        let dir = default_socket_dir().ok_or(ServerError::NoHome)?;
        Self::start_in(&dir)
    }

    /// Listens on `<dir>/<pid>.sock`, creating `dir` (owner-only) if needed
    /// and removing the stale sockets in it.
    pub fn start_in(dir: &Path) -> Result<(Self, Receiver<Event>), ServerError> {
        let io = |path: &Path| {
            let path = path.to_owned();
            move |source| ServerError::Io { path, source }
        };
        fs::create_dir_all(dir).map_err(io(dir))?;
        // Others on the machine have no business posting events.
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(io(dir))?;
        remove_stale_sockets(dir);

        let path = dir.join(format!("{}.sock", std::process::id()));
        let listener = UnixListener::bind(&path).map_err(io(&path))?;
        let (sender, receiver) = mpsc::channel();
        let stopping = Arc::new(AtomicBool::new(false));
        let rejected = Arc::new(AtomicU64::new(0));
        let thread = thread::Builder::new()
            .name("iktinos-live".to_owned())
            .spawn({
                let stopping = Arc::clone(&stopping);
                let rejected = Arc::clone(&rejected);
                move || serve(&listener, &sender, &stopping, &rejected)
            })
            .map_err(io(&path))?;
        Ok((
            Self {
                path,
                stopping,
                rejected,
                thread: Some(thread),
            },
            receiver,
        ))
    }

    /// The socket this server listens on.
    pub fn socket_path(&self) -> &Path {
        &self.path
    }

    /// How many requests were answered with an error so far: not a
    /// `POST /events`, a body that is not an event, a protocol version this
    /// crate does not speak.
    pub fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        // Wake the listener out of `accept`.
        let _ = UnixStream::connect(&self.path);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

/// The server could not start.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("no home directory to place the socket directory in")]
    NoHome,
    #[error("cannot listen at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Removes the sockets in `dir` nobody listens on. A socket that accepts the
/// connection belongs to a live consumer and is left alone.
fn remove_stale_sockets(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "sock") {
            continue;
        }
        if let Err(e) = UnixStream::connect(&path) {
            if e.kind() == io::ErrorKind::ConnectionRefused {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

fn serve(
    listener: &UnixListener,
    events: &Sender<Event>,
    stopping: &AtomicBool,
    rejected: &AtomicU64,
) {
    for stream in listener.incoming() {
        if stopping.load(Ordering::SeqCst) {
            return;
        }
        let Ok(mut stream) = stream else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
        let _ = stream.set_write_timeout(Some(READ_TIMEOUT));
        let status = match read_event(&mut stream) {
            Ok(event) => {
                if events.send(event).is_err() {
                    // Nobody receives any more.
                    return;
                }
                Status::Ok
            }
            Err(Status::Empty) => continue,
            Err(status) => {
                rejected.fetch_add(1, Ordering::Relaxed);
                status
            }
        };
        let _ = stream.write_all(status.response().as_bytes());
    }
}

/// What a request is answered with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    /// The client sent nothing: not a request, and not answered.
    Empty,
    BadRequest,
    NotFound,
    MethodNotAllowed,
    LengthRequired,
    TooLarge,
}

impl Status {
    fn response(self) -> String {
        let line = match self {
            Self::Ok => "200 OK",
            Self::BadRequest | Self::Empty => "400 Bad Request",
            Self::NotFound => "404 Not Found",
            Self::MethodNotAllowed => "405 Method Not Allowed",
            Self::LengthRequired => "411 Length Required",
            Self::TooLarge => "413 Content Too Large",
        };
        format!("HTTP/1.1 {line}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    }
}

/// Reads one `POST /events` request and parses its body.
fn read_event(stream: &mut impl Read) -> Result<Event, Status> {
    // Read until the blank line that ends the head. Whatever follows it in
    // the buffer is the beginning of the body.
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        if let Some(at) = find(&buf, b"\r\n\r\n") {
            break at;
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Err(Status::BadRequest);
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) if buf.is_empty() => return Err(Status::Empty),
            Ok(0) | Err(_) => return Err(Status::BadRequest),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let length = body_length(&buf[..head_end])?;

    let mut body = buf.split_off(head_end + 4);
    if body.len() > length {
        return Err(Status::BadRequest);
    }
    let read_so_far = body.len();
    body.resize(length, 0);
    stream
        .read_exact(&mut body[read_so_far..])
        .map_err(|_| Status::BadRequest)?;
    Event::parse(&body).map_err(|_| Status::BadRequest)
}

/// Checks the request line and returns the `Content-Length`.
fn body_length(head: &[u8]) -> Result<usize, Status> {
    let head = std::str::from_utf8(head).map_err(|_| Status::BadRequest)?;
    let mut lines = head.split("\r\n");
    let mut request = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target), Some(version), None) = (
        request.next(),
        request.next(),
        request.next(),
        request.next(),
    ) else {
        return Err(Status::BadRequest);
    };
    if !version.starts_with("HTTP/1.") {
        return Err(Status::BadRequest);
    }
    // The mod posts to `http://iktinos/events`; a client may
    // send that whole URL as the target.
    let path = target
        .strip_prefix("http://")
        .and_then(|rest| rest.find('/').map(|slash| &rest[slash..]))
        .unwrap_or(target);
    if path != "/events" {
        return Err(Status::NotFound);
    }
    if method != "POST" {
        return Err(Status::MethodNotAllowed);
    }

    let mut length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(Status::BadRequest);
        };
        if name.eq_ignore_ascii_case("content-length") {
            let value: usize = value.trim().parse().map_err(|_| Status::BadRequest)?;
            if length.is_some_and(|l| l != value) {
                return Err(Status::BadRequest);
            }
            length = Some(value);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            // Chunked bodies are not spoken here.
            return Err(Status::LengthRequired);
        }
    }
    match length {
        None => Err(Status::LengthRequired),
        Some(length) if length > MAX_BODY_BYTES => Err(Status::TooLarge),
        Some(length) => Ok(length),
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENT: &str = r#"{"v":1,"type":"turn.start","session_id":"s","ts":1,"turn_id":"t"}"#;

    fn request(head: &str, body: &str) -> Vec<u8> {
        format!("{head}\r\n\r\n{body}").into_bytes()
    }

    fn post(body: &str) -> Vec<u8> {
        request(
            &format!(
                "POST /events HTTP/1.1\r\nHost: iktinos\r\nContent-Type: application/json\r\nContent-Length: {}",
                body.len()
            ),
            body,
        )
    }

    /// Hands the request over a few bytes at a time, as a socket may.
    struct Trickle<'a>(&'a [u8]);

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.0.len().min(buf.len()).min(7);
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    #[test]
    fn a_post_of_an_event_is_read() {
        let bytes = post(EVENT);
        assert_eq!(read_event(&mut &bytes[..]).unwrap().session_id, "s");
        assert_eq!(read_event(&mut Trickle(&bytes)).unwrap().session_id, "s");
    }

    #[test]
    fn header_names_are_case_insensitive_and_an_absolute_target_is_accepted() {
        let bytes = request(
            &format!(
                "POST http://iktinos/events HTTP/1.1\r\ncontent-LENGTH:{}",
                EVENT.len()
            ),
            EVENT,
        );
        assert!(read_event(&mut &bytes[..]).is_ok());
    }

    #[test]
    fn malformed_requests_get_their_status() {
        let length = EVENT.len();
        let cases: Vec<(Vec<u8>, Status)> = vec![
            (b"garbage".to_vec(), Status::BadRequest),
            (b"\r\n\r\n".to_vec(), Status::BadRequest),
            (request("POST /events", EVENT), Status::BadRequest),
            (
                request(
                    &format!("POST /events SPDY/3\r\nContent-Length: {length}"),
                    EVENT,
                ),
                Status::BadRequest,
            ),
            (
                request(
                    &format!("GET /events HTTP/1.1\r\nContent-Length: {length}"),
                    EVENT,
                ),
                Status::MethodNotAllowed,
            ),
            (
                request(
                    &format!("POST /other HTTP/1.1\r\nContent-Length: {length}"),
                    EVENT,
                ),
                Status::NotFound,
            ),
            (
                request("POST /events HTTP/1.1\r\nHost: x", EVENT),
                Status::LengthRequired,
            ),
            (
                request("POST /events HTTP/1.1\r\nTransfer-Encoding: chunked", EVENT),
                Status::LengthRequired,
            ),
            (
                request("POST /events HTTP/1.1\r\nContent-Length: many", EVENT),
                Status::BadRequest,
            ),
            (
                request(
                    &format!(
                        "POST /events HTTP/1.1\r\nContent-Length: {}",
                        MAX_BODY_BYTES + 1
                    ),
                    EVENT,
                ),
                Status::TooLarge,
            ),
            // Shorter and longer than announced.
            (
                request(
                    &format!("POST /events HTTP/1.1\r\nContent-Length: {}", length + 5),
                    EVENT,
                ),
                Status::BadRequest,
            ),
            (
                request(
                    &format!("POST /events HTTP/1.1\r\nContent-Length: {}", length - 5),
                    EVENT,
                ),
                Status::BadRequest,
            ),
            (post("not an event"), Status::BadRequest),
            (
                post(r#"{"v":2,"type":"turn.start","session_id":"s","ts":1,"turn_id":"t"}"#),
                Status::BadRequest,
            ),
            // A head that never ends.
            (
                format!("POST /events HTTP/1.1\r\n{}", "X: y\r\n".repeat(2000)).into_bytes(),
                Status::BadRequest,
            ),
        ];
        for (bytes, expected) in cases {
            let shown = String::from_utf8_lossy(&bytes[..bytes.len().min(60)]).into_owned();
            assert_eq!(
                read_event(&mut &bytes[..]).unwrap_err(),
                expected,
                "{shown}"
            );
        }
    }

    #[test]
    fn the_largest_body_is_accepted() {
        let padding = "x".repeat(MAX_BODY_BYTES - EVENT.len() - r#","pad":"""#.len());
        let body = format!(r#"{},"pad":"{padding}"}}"#, &EVENT[..EVENT.len() - 1]);
        assert_eq!(body.len(), MAX_BODY_BYTES);
        assert!(read_event(&mut Trickle(&post(&body))).is_ok());
    }
}
