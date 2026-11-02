//! A small HTTP/1.1 client: one request per connection, plain or TLS, with Basic, Digest or
//! API-key authentication, chunked or sized replies (capped), and multipart uploads.

use std::io::{BufRead, BufReader, Read, Write};
use std::time::Duration;

use base64::Engine;

use crate::net::{self, Trust};

/// Largest reply read (a camera snapshot or a file list).
const MAX_BODY: usize = 64 << 20;
/// Longest header block.
const MAX_HEADERS: usize = 64 << 10;

/// A URL split into what a request needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    /// Path and query, starting with `/`.
    pub path: String,
}

impl Url {
    /// Reads `http://host:port/path`, `https://…`, or a bare `host[:port][/path]` (http).
    pub fn parse(text: &str) -> Result<Url, String> {
        let text = text.trim();
        let (tls, rest) = if let Some(r) = text.strip_prefix("https://") {
            (true, r)
        } else if let Some(r) = text.strip_prefix("http://") {
            (false, r)
        } else if text.contains("://") {
            return Err(format!("{text}: only http and https addresses are supported"));
        } else {
            (false, text)
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (rest.get(..i).unwrap_or(""), rest.get(i..).unwrap_or("/")),
            None => (rest, "/"),
        };
        // user:pass@ is not used; drop it rather than send it as the host.
        let authority = authority.rsplit('@').next().unwrap_or(authority);
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (h, after) = v6.split_once(']').ok_or_else(|| format!("{text}: bad address"))?;
            (h.to_owned(), after.strip_prefix(':'))
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_owned(), Some(p)),
                None => (authority.to_owned(), None),
            }
        };
        if host.is_empty() {
            return Err("the printer's address is empty".into());
        }
        let port = match port {
            Some(p) => p.parse::<u16>().map_err(|_| format!("{text}: bad port"))?,
            None if tls => 443,
            None => 80,
        };
        Ok(Url { tls, host, port, path: path.to_owned() })
    }

    /// This server with another path.
    pub fn with_path(&self, path: &str) -> Url {
        let base = self.path.trim_end_matches('/');
        let path = if path.starts_with('/') { path.to_owned() } else { format!("/{path}") };
        // A base path (a printer behind a proxy at /printer) is kept in front.
        let full = if base.is_empty() || path.starts_with(base) { path } else { format!("{base}{path}") };
        Url { path: full, ..self.clone() }
    }

    fn host_header(&self) -> String {
        let default = if self.tls { 443 } else { 80 };
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        if self.port == default { host } else { format!("{host}:{}", self.port) }
    }
}

/// How to sign requests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Auth {
    #[default]
    None,
    /// An `X-Api-Key` header (OctoPrint, Moonraker, PrusaLink).
    ApiKey(String),
    /// Basic, or Digest when the server asks for it.
    User(String, String),
}

#[derive(Clone, Debug)]
pub struct Request {
    pub method: &'static str,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub auth: Auth,
    pub trust: Trust,
    pub timeout: Duration,
}

impl Request {
    pub fn new(method: &'static str, url: Url) -> Request {
        Request {
            method,
            url,
            headers: Vec::new(),
            body: Vec::new(),
            auth: Auth::None,
            trust: Trust::Any,
            timeout: Duration::from_secs(10),
        }
    }

    pub fn header(mut self, k: &str, v: &str) -> Request {
        self.headers.push((k.to_owned(), v.to_owned()));
        self
    }

    pub fn body(mut self, content_type: &str, body: Vec<u8>) -> Request {
        self.headers.push(("Content-Type".into(), content_type.to_owned()));
        self.body = body;
        self
    }

    pub fn json(self, value: &serde_json::Value) -> Request {
        self.body("application/json", value.to_string().into_bytes())
    }

    pub fn multipart(self, form: Multipart) -> Request {
        let (ct, body) = form.finish();
        self.body(&ct, body)
    }

    pub fn auth(mut self, auth: Auth) -> Request {
        self.auth = auth;
        self
    }

    pub fn timeout(mut self, t: Duration) -> Request {
        self.timeout = t;
        self
    }

    /// Sends the request; a Digest challenge is answered once. Statuses of 400 and over are
    /// errors with the server's text.
    pub fn send(&self) -> Result<Response, String> {
        let first = self.send_once(None)?;
        if first.status == 401
            && let Auth::User(user, pass) = &self.auth
            && let Some(challenge) = first.header("www-authenticate").filter(|c| c.trim_start().starts_with("Digest"))
        {
            let answer = digest_answer(challenge, user, pass, self.method, &self.url.path);
            return self.send_once(Some(answer))?.ok();
        }
        first.ok()
    }

    fn send_once(&self, authorization: Option<String>) -> Result<Response, String> {
        let url = &self.url;
        let mut stream = net::connect(&url.host, url.port, url.tls.then_some(self.trust), self.timeout)
            .map_err(|e| format!("could not connect to {}: {e}", url.host))?;
        let mut head = format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: CosmicSlicer\r\nAccept: */*\r\nConnection: close\r\n",
            self.method,
            url.path,
            url.host_header()
        );
        match (&self.auth, authorization) {
            (_, Some(a)) => head.push_str(&format!("Authorization: {a}\r\n")),
            (Auth::ApiKey(k), None) if !k.is_empty() => head.push_str(&format!("X-Api-Key: {k}\r\n")),
            (Auth::User(u, p), None) if !u.is_empty() => {
                let token = base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"));
                head.push_str(&format!("Authorization: Basic {token}\r\n"));
            }
            _ => {}
        }
        for (k, v) in &self.headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        if !self.body.is_empty() || matches!(self.method, "POST" | "PUT") {
            head.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
        }
        head.push_str("\r\n");
        let io = |e: std::io::Error| format!("{}: {e}", url.host);
        stream.write_all(head.as_bytes()).map_err(io)?;
        stream.write_all(&self.body).map_err(io)?;
        stream.flush().map_err(io)?;
        read_response(BufReader::new(stream)).map_err(|e| format!("{}: {e}", url.host))
    }
}

#[derive(Clone, Debug, Default)]
pub struct Response {
    pub status: u16,
    /// Header names in lower case.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> Result<serde_json::Value, String> {
        serde_json::from_slice(&self.body).map_err(|e| format!("the printer's reply is not JSON: {e}"))
    }

    fn ok(self) -> Result<Response, String> {
        if self.status < 400 {
            return Ok(self);
        }
        let text = self.text();
        let short: String = text.trim().chars().take(200).collect();
        Err(match self.status {
            401 | 403 => format!("the printer refused the key or password ({})", self.status),
            404 => "the printer does not offer this (404); check the host type".into(),
            s if short.is_empty() => format!("the printer answered {s}"),
            s => format!("the printer answered {s}: {short}"),
        })
    }
}

/// Reads one line into `line`, counting it against the header budget.
fn read_line(r: &mut impl BufRead, line: &mut String, used: &mut usize) -> Result<(), String> {
    line.clear();
    let n = r.take(MAX_HEADERS as u64).read_line(line).map_err(|e| e.to_string())?;
    *used += n;
    if *used > MAX_HEADERS {
        return Err("the reply's headers are too long".into());
    }
    Ok(())
}

/// Reads a reply: status line, headers, then a sized, chunked or until-close body.
pub fn read_response(mut r: impl BufRead) -> Result<Response, String> {
    let mut line = String::new();
    let mut header_bytes = 0;
    read_line(&mut r, &mut line, &mut header_bytes)?;
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|_| line.starts_with("HTTP/"))
        .ok_or_else(|| format!("not an HTTP reply: {}", line.trim().chars().take(60).collect::<String>()))?;
    let mut headers = Vec::new();
    loop {
        read_line(&mut r, &mut line, &mut header_bytes)?;
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
        }
    }
    let get = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    let mut body = Vec::new();
    if get("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        loop {
            header_bytes = 0;
            read_line(&mut r, &mut line, &mut header_bytes).map_err(|e| format!("in a chunk: {e}"))?;
            let size_text = line.trim().split(';').next().unwrap_or("");
            let size = usize::from_str_radix(size_text, 16).map_err(|_| "a bad chunk in the reply".to_owned())?;
            if size == 0 {
                break;
            }
            if body.len() + size > MAX_BODY {
                return Err("the reply is too large".into());
            }
            let start = body.len();
            body.resize(start + size, 0);
            r.read_exact(body.get_mut(start..).unwrap_or(&mut [])).map_err(|e| e.to_string())?;
            let mut crlf = [0u8; 2];
            let _ = r.read_exact(&mut crlf);
        }
    } else if let Some(len) = get("content-length").and_then(|v| v.parse::<usize>().ok()) {
        if len > MAX_BODY {
            return Err("the reply is too large".into());
        }
        body.resize(len, 0);
        r.read_exact(&mut body).map_err(|e| e.to_string())?;
    } else if status != 204 && status != 304 {
        r.take(MAX_BODY as u64 + 1).read_to_end(&mut body).map_err(|e| e.to_string())?;
        if body.len() > MAX_BODY {
            return Err("the reply is too large".into());
        }
    }
    Ok(Response { status, headers, body })
}

/// A `multipart/form-data` body.
#[derive(Default)]
pub struct Multipart {
    parts: Vec<u8>,
    boundary: String,
}

impl Multipart {
    pub fn new() -> Multipart {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        Multipart { parts: Vec::new(), boundary: format!("----CosmicSlicer{nanos:x}") }
    }

    pub fn field(mut self, name: &str, value: &str) -> Multipart {
        self.parts.extend_from_slice(
            format!("--{}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n", self.boundary)
                .as_bytes(),
        );
        self
    }

    pub fn file(mut self, name: &str, filename: &str, bytes: &[u8]) -> Multipart {
        let filename = filename.replace(['"', '\r', '\n'], "_");
        self.parts.extend_from_slice(
            format!(
                "--{}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n",
                self.boundary
            )
            .as_bytes(),
        );
        self.parts.extend_from_slice(bytes);
        self.parts.extend_from_slice(b"\r\n");
        self
    }

    fn finish(mut self) -> (String, Vec<u8>) {
        self.parts.extend_from_slice(format!("--{}--\r\n", self.boundary).as_bytes());
        (format!("multipart/form-data; boundary={}", self.boundary), self.parts)
    }
}

/// The `Authorization` value answering a Digest `challenge` (RFC 7616, MD5, qop auth).
fn digest_answer(challenge: &str, user: &str, pass: &str, method: &str, uri: &str) -> String {
    let rest = challenge.trim_start().trim_start_matches("Digest").trim();
    let mut fields = std::collections::HashMap::new();
    for part in split_params(rest) {
        if let Some((k, v)) = part.split_once('=') {
            fields.insert(k.trim().to_ascii_lowercase(), v.trim().trim_matches('"').to_owned());
        }
    }
    let get = |k: &str| fields.get(k).cloned().unwrap_or_default();
    let (realm, nonce, opaque, qop) = (get("realm"), get("nonce"), get("opaque"), get("qop"));
    let ha1 = crate::md5::hex(format!("{user}:{realm}:{pass}").as_bytes());
    let ha2 = crate::md5::hex(format!("{method}:{uri}").as_bytes());
    let cnonce = format!(
        "{:016x}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64)
    );
    let mut out =
        format!("Digest username=\"{user}\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"{uri}\", algorithm=MD5");
    if qop.split(',').any(|q| q.trim() == "auth") {
        let response = crate::md5::hex(format!("{ha1}:{nonce}:00000001:{cnonce}:auth:{ha2}").as_bytes());
        out.push_str(&format!(", qop=auth, nc=00000001, cnonce=\"{cnonce}\", response=\"{response}\""));
    } else {
        let response = crate::md5::hex(format!("{ha1}:{nonce}:{ha2}").as_bytes());
        out.push_str(&format!(", response=\"{response}\""));
    }
    if !opaque.is_empty() {
        out.push_str(&format!(", opaque=\"{opaque}\""));
    }
    out
}

/// Splits `a="x, y", b=z` at the commas outside quotes.
fn split_params(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// `text` made safe for a URL query or path segment.
pub fn encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A one-shot server on a free port: answers each connection with `reply(request)`,
    /// `count` times. Returns its address and the requests it saw.
    pub fn serve(
        count: usize,
        reply: impl Fn(&str, &[u8]) -> Vec<u8> + Send + 'static,
    ) -> (String, std::sync::mpsc::Receiver<(String, Vec<u8>)>) {
        let listener = TcpListener::bind("127.0.0.1:0").ok();
        let addr = listener.as_ref().and_then(|l| l.local_addr().ok()).map_or(String::new(), |a| a.to_string());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Some(listener) = listener else { return };
            for _ in 0..count {
                let Ok((mut s, _)) = listener.accept() else { return };
                let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                let Ok(clone) = s.try_clone() else { continue };
                let mut r = BufReader::new(clone);
                let mut head = String::new();
                let mut len = 0usize;
                loop {
                    let mut l = String::new();
                    if r.read_line(&mut l).unwrap_or(0) == 0 {
                        break;
                    }
                    if let Some(v) = l.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    head.push_str(&l);
                    if l == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0u8; len];
                let _ = r.read_exact(&mut body);
                let out = reply(&head, &body);
                let _ = s.write_all(&out);
                let _ = tx.send((head, body));
            }
        });
        (addr, rx)
    }

    pub fn ok_json(v: &str) -> Vec<u8> {
        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{v}", v.len())
            .into_bytes()
    }

    #[test]
    fn urls() -> Result<(), String> {
        let u = Url::parse("192.168.1.5")?;
        assert_eq!((u.tls, u.host.as_str(), u.port, u.path.as_str()), (false, "192.168.1.5", 80, "/"));
        let u = Url::parse("https://printer.local:7125/base/")?;
        assert_eq!((u.tls, u.port, u.with_path("/api/version").path.as_str()), (true, 7125, "/base/api/version"));
        assert!(Url::parse("ftp://x").is_err());
        assert!(Url::parse("").is_err());
        assert!(Url::parse("host:99999").is_err());
        assert_eq!(encode("a b/ü"), "a%20b%2F%C3%BC");
        Ok(())
    }

    #[test]
    fn chunked_and_sized_replies() {
        let r = read_response(
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n"[..],
        );
        assert_eq!(r.map(|r| r.text()).ok().as_deref(), Some("hello world"));
        let r = read_response(&b"HTTP/1.0 404 Not Found\r\nContent-Length: 2\r\n\r\nno"[..]);
        assert!(r.is_ok_and(|r| r.status == 404 && r.ok().is_err()));
        assert!(read_response(&b"garbage"[..]).is_err());
        assert!(read_response(&b"HTTP/1.1 200 OK\r\nContent-Length: 999999999999\r\n\r\n"[..]).is_err());
    }

    #[test]
    fn requests_reach_a_server_with_digest_auth() -> Result<(), String> {
        let (addr, rx) = serve(2, |head, _| {
            if head.contains("Authorization: Digest") {
                ok_json("{\"ok\":true}")
            } else {
                b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"Printer\", nonce=\"abc\", qop=\"auth\"\r\nContent-Length: 0\r\n\r\n".to_vec()
            }
        });
        let url = Url::parse(&format!("http://{addr}/api/version"))?;
        let r = Request::new("GET", url).auth(Auth::User("maker".into(), "secret".into())).send();
        assert!(r.as_ref().is_ok_and(|r| r.json().is_ok_and(|j| j["ok"] == true)), "{r:?}");
        let seen: Vec<String> = rx.try_iter().map(|(h, _)| h).collect();
        assert!(seen.iter().any(|h| h.contains("username=\"maker\"") && h.contains("realm=\"Printer\"")), "{seen:?}");
        Ok(())
    }
}
