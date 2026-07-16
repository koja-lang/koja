//! Minimal blocking HTTP/1.1 static file server for `koja doc
//! serve`. Hand-rolled so the driver doesn't grow a new crate
//! dependency just to preview the generated doc tree. Single-
//! threaded (one user, mostly-cached) with `Connection: close`
//! so we don't need keep-alive bookkeeping.
//!
//! Path resolution canonicalizes the doc root once at startup
//! and refuses any request whose normalized URL would escape it,
//! so the `..` segments arriving over the wire can't reach
//! outside the doc tree. URLs ending in `/` are rewritten to
//! the directory's `index.html`.

use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// First port we'll try when the caller didn't pin one.
const DEFAULT_PORT: u16 = 8000;
/// Highest port we'll attempt before giving up. Keeps the
/// search window bounded so a misconfigured host doesn't make
/// `koja doc serve` walk every port.
const MAX_PORT_PROBE: u16 = 8019;

/// How long any single client request gets to finish its
/// initial line + headers. Generous enough for slow loopback
/// while still bounded.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Start serving `doc_root` on `127.0.0.1`. With `port = Some(p)`
/// we error out if the port is taken. With `port = None` we
/// probe upward from [`DEFAULT_PORT`] until [`MAX_PORT_PROBE`].
/// Returns only when the listener fails or the process is
/// signaled, since accepts are handled inline.
pub fn run(doc_root: &Path, port: Option<u16>) -> Result<(), ServeError> {
    let canonical_root = doc_root
        .canonicalize()
        .map_err(|e| ServeError::DocRoot(doc_root.to_path_buf(), e))?;
    let listener = bind_listener(port)?;
    let addr = listener.local_addr().map_err(ServeError::Bind)?;

    println!(
        "serving {} on http://{} (Ctrl-C to stop)",
        canonical_root.display(),
        addr
    );

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(e) = handle_client(stream, &canonical_root) {
                    eprintln!("warning: serve: {e}");
                }
            }
            Err(e) => eprintln!("warning: accept error: {e}"),
        }
    }
    Ok(())
}

/// Reasons the server can fail to start or handle a request.
/// `koja doc serve` exits with a friendly message rather than a
/// raw `io::Error` debug print.
#[derive(Debug)]
pub enum ServeError {
    /// `doc_root` couldn't be canonicalized (typically: dir
    /// missing because the user passed `--no-rebuild` on a
    /// fresh project).
    DocRoot(PathBuf, io::Error),
    /// All probed ports were taken.
    NoFreePort { start: u16, end: u16 },
    /// `TcpListener::bind` failed for a fixed port.
    Bind(io::Error),
}

impl fmt::Display for ServeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServeError::DocRoot(p, e) => {
                write!(
                    f,
                    "cannot serve {}: {e} (try running `koja doc` first)",
                    p.display()
                )
            }
            ServeError::NoFreePort { start, end } => {
                write!(f, "no free TCP port found in {start}..={end}")
            }
            ServeError::Bind(e) => write!(f, "cannot bind listener: {e}"),
        }
    }
}

impl Error for ServeError {}

fn bind_listener(port: Option<u16>) -> Result<TcpListener, ServeError> {
    if let Some(p) = port {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), p);
        return TcpListener::bind(addr).map_err(ServeError::Bind);
    }
    for p in DEFAULT_PORT..=MAX_PORT_PROBE {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), p);
        if let Ok(listener) = TcpListener::bind(addr) {
            return Ok(listener);
        }
    }
    Err(ServeError::NoFreePort {
        start: DEFAULT_PORT,
        end: MAX_PORT_PROBE,
    })
}

fn handle_client(mut stream: TcpStream, doc_root: &Path) -> io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(READ_TIMEOUT))?;

    let request_line = match read_request_head(&mut stream)? {
        Some(line) => line,
        None => return Ok(()),
    };

    let Some(path) = parse_request_path(&request_line) else {
        return write_status(&mut stream, 400, "Bad Request", b"bad request");
    };

    match resolve(doc_root, &path) {
        Some(file_path) => serve_file(&mut stream, &file_path),
        None => write_status(&mut stream, 404, "Not Found", b"404 not found"),
    }
}

/// Read the full HTTP request head (up to `\r\n\r\n`) and
/// return just the request line. Draining the header block
/// before responding matters on macOS, which sends RST instead
/// of FIN when a socket is closed with unread bytes buffered.
/// The client would then see "connection reset by peer" before
/// it finishes reading the body. Returns `Ok(None)` on a clean
/// EOF (empty connection, e.g. a port probe).
fn read_request_head(stream: &mut TcpStream) -> io::Result<Option<String>> {
    let mut buf = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte)? {
            0 => {
                if buf.is_empty() {
                    return Ok(None);
                }
                break;
            }
            _ => {
                buf.push(byte[0]);
                if buf.ends_with(b"\r\n\r\n") || buf.ends_with(b"\n\n") {
                    break;
                }
                if buf.len() > 8192 {
                    break;
                }
            }
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let line = text.lines().next().unwrap_or("").to_string();
    Ok(Some(line))
}

/// Extract the URL path from a request line like `GET /foo HTTP/1.1`.
/// Returns `None` for non-GET methods or malformed lines so the
/// caller can respond `400`.
pub(crate) fn parse_request_path(line: &str) -> Option<String> {
    let mut parts = line.splitn(3, ' ');
    let method = parts.next()?;
    let url = parts.next()?;
    let _version = parts.next()?;
    if method != "GET" && method != "HEAD" {
        return None;
    }
    let path = url.split('?').next().unwrap_or(url);
    Some(path.to_string())
}

/// Resolve a URL path under `doc_root` to a concrete file path,
/// rewriting directories to their `index.html`. Rejects any
/// request whose normalized path escapes the root (any `..`
/// component beyond the start). Returns `None` for misses so the
/// caller can 404.
pub(crate) fn resolve(doc_root: &Path, url_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(url_path);
    let trimmed = decoded.trim_start_matches('/');
    let mut path = PathBuf::from(doc_root);
    let mut depth: i32 = 0;
    for component in Path::new(trimmed).components() {
        match component {
            Component::Normal(seg) => {
                path.push(seg);
                depth += 1;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
                path.pop();
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }

    if path.is_dir() {
        path.push("index.html");
    }

    if !path.starts_with(doc_root) || !path.is_file() {
        return None;
    }
    Some(path)
}

fn serve_file(stream: &mut TcpStream, path: &Path) -> io::Result<()> {
    let bytes = fs::read(path)?;
    let mime = mime_type(path);
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()
}

fn write_status(stream: &mut TcpStream, code: u16, reason: &str, body: &[u8]) -> io::Result<()> {
    let header = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// Map a file extension to a sensible Content-Type. Covers every
/// asset the doc generator emits today (html / css / js / json)
/// plus a couple of common image fallbacks. Falls back to
/// `application/octet-stream` for anything else so the browser
/// at least won't sniff into the wrong handler.
pub(crate) fn mime_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") | Some("mjs") => "application/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("ico") => "image/x-icon",
        Some("txt") | Some("md") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Minimal percent-decoder for URL paths. Only handles the
/// `%HH` form the doc tree could realistically produce (spaces,
/// unicode chars in file names). Bad escapes pass through
/// untouched rather than erroring. The worst case is a 404
/// when [`resolve`] then fails to find the literal path.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_request_path_extracts_url() {
        assert_eq!(
            parse_request_path("GET /index.html HTTP/1.1"),
            Some("/index.html".to_string())
        );
        assert_eq!(
            parse_request_path("GET /Crypto/SHA256.html?foo=1 HTTP/1.1"),
            Some("/Crypto/SHA256.html".to_string())
        );
        assert_eq!(
            parse_request_path("HEAD /search.js HTTP/1.0"),
            Some("/search.js".to_string())
        );
        assert_eq!(parse_request_path("POST /x HTTP/1.1"), None);
        assert_eq!(parse_request_path("garbage"), None);
    }

    #[test]
    fn mime_type_covers_doc_assets() {
        assert_eq!(
            mime_type(Path::new("index.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(mime_type(Path::new("style.css")), "text/css; charset=utf-8");
        assert_eq!(
            mime_type(Path::new("search.js")),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(
            mime_type(Path::new("search-index.json")),
            "application/json; charset=utf-8"
        );
        assert_eq!(
            mime_type(Path::new("unknown.bin")),
            "application/octet-stream"
        );
    }

    #[test]
    fn resolve_serves_index_html_for_dirs_and_rejects_traversal() {
        let tmp = std::env::temp_dir().join(format!(
            "koja-serve-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pkg = tmp.join("Crypto");
        fs::create_dir_all(&pkg).unwrap();
        fs::write(tmp.join("index.html"), "root").unwrap();
        fs::write(pkg.join("index.html"), "crypto").unwrap();
        fs::write(pkg.join("SHA256.html"), "sha").unwrap();
        let root = tmp.canonicalize().unwrap();

        assert_eq!(resolve(&root, "/"), Some(root.join("index.html")));
        assert_eq!(
            resolve(&root, "/Crypto/"),
            Some(root.join("Crypto").join("index.html"))
        );
        assert_eq!(
            resolve(&root, "/Crypto/SHA256.html"),
            Some(root.join("Crypto").join("SHA256.html"))
        );
        assert!(resolve(&root, "/missing.html").is_none());
        assert!(
            resolve(&root, "/../etc/passwd").is_none(),
            "must reject traversal above root"
        );
        assert!(
            resolve(&root, "/Crypto/../../etc/passwd").is_none(),
            "must reject traversal across segments"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn percent_decode_handles_escaped_chars() {
        assert_eq!(percent_decode("/foo%20bar"), "/foo bar");
        assert_eq!(percent_decode("/%2F"), "//");
        assert_eq!(percent_decode("/plain"), "/plain");
        assert_eq!(percent_decode("/bad%ZZ"), "/bad%ZZ");
    }
}
