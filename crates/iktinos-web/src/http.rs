//! Just enough HTTP/1.1 to answer a browser on the same machine: one `GET`
//! per connection, a request line and headers, no body.

use std::io::{self, Read, Write};

/// The largest request head (request line and headers) accepted.
pub const MAX_HEAD_BYTES: usize = 16 * 1024;

/// A request: its line and its headers. A body, if one was sent, is not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    /// The path, without the query.
    pub path: String,
    /// What follows `?`, as it was sent.
    pub query: Option<String>,
    /// Names lower-cased, values trimmed.
    headers: Vec<(String, String)>,
}

impl Request {
    /// The value of a header, whatever the case its name was sent in.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }

    /// The value of a parameter of the query. Values are taken as they are:
    /// nothing this server reads from a query needs decoding.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.query
            .as_deref()?
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value)
    }

    /// The value of a cookie.
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.header("cookie")?
            .split(';')
            .filter_map(|pair| pair.trim().split_once('='))
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value)
    }
}

/// Why a request could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Malformed {
    /// The client sent nothing: not a request.
    Empty,
    /// Not a request this server reads.
    Bad,
    /// The head is longer than [`MAX_HEAD_BYTES`].
    TooLarge,
}

/// Reads one request's head.
pub fn read_request(stream: &mut impl Read) -> Result<Request, Malformed> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        if let Some(at) = find(&buf, b"\r\n\r\n") {
            break at;
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Err(Malformed::TooLarge);
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) if buf.is_empty() => return Err(Malformed::Empty),
            Ok(0) | Err(_) => return Err(Malformed::Bad),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head = std::str::from_utf8(&buf[..head_end]).map_err(|_| Malformed::Bad)?;
    let mut lines = head.split("\r\n");
    let mut line = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (line.next(), line.next(), line.next(), line.next())
    else {
        return Err(Malformed::Bad);
    };
    if !version.starts_with("HTTP/1.") || !target.starts_with('/') {
        return Err(Malformed::Bad);
    }
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, Some(query.to_owned())),
        None => (target, None),
    };
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(Malformed::Bad)?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    Ok(Request {
        method: method.to_owned(),
        path: path.to_owned(),
        query,
        headers,
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// What a request is answered with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    /// Further headers: a cookie, a redirect, what a method may be.
    pub headers: Vec<(&'static str, String)>,
}

impl Response {
    pub fn new(status: u16, content_type: &'static str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            content_type,
            body: body.into(),
            headers: Vec::new(),
        }
    }

    /// A line of text: what an error says.
    pub fn text(status: u16, text: &str) -> Self {
        Self::new(status, "text/plain; charset=utf-8", format!("{text}\n"))
    }

    pub fn json(body: Vec<u8>) -> Self {
        Self::new(200, "application/json", body)
    }

    pub fn with(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    /// Writes the response and nothing after it: the connection is closed.
    pub fn write(&self, stream: &mut impl Write) -> io::Result<()> {
        let mut head = head(self.status, self.content_type);
        head.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
        for (name, value) in &self.headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes())?;
        stream.write_all(&self.body)?;
        stream.flush()
    }
}

/// The status line and the headers every response carries. The page may
/// load and run only what this server serves, may talk only to this server,
/// and may not be framed: text from a transcript that got onto the page as
/// markup still could not run or call out.
pub fn head(status: u16, content_type: &str) -> String {
    let reason = match status {
        200 => "OK",
        303 => "See Other",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Connection: close\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n\
         Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; \
         img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; \
         form-action 'none'; frame-ancestors 'none'\r\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(text: &str) -> Result<Request, Malformed> {
        read_request(&mut text.as_bytes())
    }

    #[test]
    fn a_request_is_read_with_its_query_headers_and_cookies() {
        let request = request(
            "GET /api/sessions?token=abc&x=1 HTTP/1.1\r\nHost: 127.0.0.1:4747\r\n\
             COOKIE: other=1; iktinos=secret ;last=2\r\n\r\n",
        )
        .unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/api/sessions");
        assert_eq!(request.param("token"), Some("abc"));
        assert_eq!(request.param("x"), Some("1"));
        assert_eq!(request.param("y"), None);
        assert_eq!(request.header("host"), Some("127.0.0.1:4747"));
        assert_eq!(request.cookie("iktinos"), Some("secret"));
        assert_eq!(request.cookie("last"), Some("2"));
        assert_eq!(request.cookie("none"), None);

        let plain = self::request("GET / HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!((plain.path.as_str(), plain.query.as_deref()), ("/", None));
        assert_eq!(plain.cookie("iktinos"), None);
    }

    #[test]
    fn what_is_not_a_request_is_told_apart() {
        assert_eq!(request(""), Err(Malformed::Empty));
        for bad in [
            "garbage",
            "GET /\r\n\r\n",
            "GET / SPDY/3\r\n\r\n",
            "GET http://elsewhere/ HTTP/1.1\r\n\r\n",
            "GET / HTTP/1.1\r\nno colon\r\n\r\n",
            "GET / HTTP/1.1 extra\r\n\r\n",
        ] {
            assert_eq!(request(bad), Err(Malformed::Bad), "{bad}");
        }
        let long = format!("GET / HTTP/1.1\r\n{}", "X: y\r\n".repeat(4000));
        assert_eq!(request(&long), Err(Malformed::TooLarge));
    }

    #[test]
    fn a_response_closes_the_connection_and_confines_the_page() {
        let mut out = Vec::new();
        Response::text(404, "no such session")
            .with("X-Test", "1")
            .write(&mut out)
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 404 Not Found\r\n"), "{text}");
        assert!(text.contains("Content-Length: 16\r\n"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(text.contains("Cache-Control: no-store\r\n"));
        assert!(text.contains("X-Content-Type-Options: nosniff\r\n"));
        assert!(text.contains("script-src 'self'"));
        assert!(text.contains("connect-src 'self'"));
        assert!(text.contains("frame-ancestors 'none'"));
        assert!(!text.contains("unsafe-inline"));
        assert!(text.contains("X-Test: 1\r\n"));
        assert!(text.ends_with("\r\n\r\nno such session\n"));
    }
}
