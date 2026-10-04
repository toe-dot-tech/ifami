//! A minimal HTTP/1.1 origin server on `127.0.0.1`, for integration tests.
//!
//! Deliberately hand-rolled rather than pulled from a crate: these tests exist
//! to prove the engine talks to a real socket over real HTTP/1.1 framing, and a
//! framework in the middle would let a bug in our own framing assumptions hide
//! behind the framework's.
//!
//! Everything binds loopback on an ephemeral port. Nothing here may ever bind a
//! routable interface; `assert_loopback` fails the test if that changes.
//!
//! # Why this is not in `tests/loopback.rs`
//!
//! It is shared by every integration test in this directory, and a directory
//! module is not compiled as its own test binary.

#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// How one path should be served.
#[derive(Clone)]
pub struct Route {
    body: Vec<u8>,
    content_type: String,
    /// Advertise `Accept-Ranges: bytes` and honour `Range`.
    ranges: bool,
    /// Serve `200 OK` with the whole body even when `Range` was requested.
    ///
    /// Models a CDN node or a misconfigured origin. The engine must detect this
    /// and restart rather than append a second copy of the file to the first.
    ignore_range: bool,
    /// Send only this many body bytes, then drop the connection.
    ///
    /// Models the single most common real-world failure: the transfer looked
    /// fine and then stopped. `Content-Length` still promises the full length,
    /// which is exactly what makes this a mid-stream error rather than a
    /// short-body success.
    cut_after: Option<usize>,
    /// Overrides the status code. Used to produce `401`/`403` gates.
    status: Option<u16>,
}

impl Route {
    /// A `200`-only route with the given body and content type.
    pub fn bytes(body: impl Into<Vec<u8>>, content_type: &str) -> Self {
        Route {
            body: body.into(),
            content_type: content_type.to_string(),
            ranges: false,
            ignore_range: false,
            cut_after: None,
            status: None,
        }
    }

    /// A text route.
    pub fn text(body: &str, content_type: &str) -> Self {
        Self::bytes(body.as_bytes().to_vec(), content_type)
    }

    /// Advertise and honour `Range`.
    pub fn resumable(mut self) -> Self {
        self.ranges = true;
        self
    }

    /// Advertise `Accept-Ranges: bytes` but ignore the `Range` header.
    pub fn refusing_range(mut self) -> Self {
        self.ranges = true;
        self.ignore_range = true;
        self
    }

    /// Cut the body short after `n` bytes, keeping the full `Content-Length`.
    pub fn cut_after(mut self, n: usize) -> Self {
        self.cut_after = Some(n);
        self
    }

    /// Force a status code.
    pub fn status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }
}

/// One recorded request.
#[derive(Debug, Clone)]
pub struct Hit {
    pub method: String,
    pub path: String,
    /// The raw `Range` header value, if any.
    pub range: Option<String>,
}

impl Hit {
    /// The byte offset this request asked to start from, if it asked.
    pub fn range_start(&self) -> Option<u64> {
        let raw = self.range.as_deref()?;
        let spec = raw.strip_prefix("bytes=")?;
        let start = spec.split('-').next()?;
        start.parse().ok()
    }

    /// Whether this request carried a `Range` header at all.
    pub fn was_range(&self) -> bool {
        self.range.is_some()
    }
}

/// Body returned for an unrouted path.
const NOT_FOUND: &[u8] = b"not found";
/// Body returned for a status-gated route.
const GATED: &[u8] = b"gated";

struct State {
    routes: Mutex<HashMap<String, Route>>,
    log: Mutex<Vec<Hit>>,
}

impl State {
    fn record(&self, hit: Hit) {
        self.log.lock().unwrap().push(hit);
    }

    fn hits(&self) -> Vec<Hit> {
        self.log.lock().unwrap().clone()
    }
}

/// A running fixture server.
pub struct Fixture {
    addr: SocketAddr,
    state: Arc<State>,
}

impl Fixture {
    /// Start a server with no routes.
    pub async fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind loopback fixture server");
        let addr = listener.local_addr().expect("local_addr");
        assert_loopback(addr);

        let state = Arc::new(State {
            routes: Mutex::new(HashMap::new()),
            log: Mutex::new(Vec::new()),
        });

        let accept_state = Arc::clone(&state);
        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let s = Arc::clone(&accept_state);
                // One task per connection. The server always says
                // `Connection: close`, so there is no keep-alive to manage.
                tokio::spawn(async move {
                    let _ = serve_connection(socket, s).await;
                });
            }
        });

        Fixture { addr, state }
    }

    /// Register a route. Replaces any existing route for the same path.
    pub fn route(&self, path: &str, route: Route) {
        assert!(path.starts_with('/'), "route paths need a leading slash");
        self.state
            .routes
            .lock()
            .unwrap()
            .insert(path.to_string(), route);
    }

    /// Remove a route, so the path starts returning `404`.
    pub fn unroute(&self, path: &str) {
        self.state.routes.lock().unwrap().remove(path);
    }

    /// Every request the server has served.
    pub fn hits(&self) -> Vec<Hit> {
        self.state.hits()
    }

    /// Requests for `path` only.
    pub fn hits_for(&self, path: &str) -> Vec<Hit> {
        self.hits().into_iter().filter(|h| h.path == path).collect()
    }

    /// Absolute `http://127.0.0.1:PORT/path` URL for `path`.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }
}

fn assert_loopback(addr: SocketAddr) {
    assert!(
        addr.ip().is_loopback(),
        "integration fixtures must never bind {addr}: a routable listener in a \
         test suite is a security regression, not a convenience"
    );
}

async fn serve_connection(mut socket: TcpStream, state: Arc<State>) -> std::io::Result<()> {
    let request = read_request(&mut socket).await?;
    if let Some((method, path, range)) = request {
        return dispatch(&mut socket, state, &method, &path, range.as_deref()).await;
    }
    Ok(())
}

/// Serve one parsed request.
async fn dispatch(
    socket: &mut TcpStream,
    state: Arc<State>,
    method: &str,
    path: &str,
    range: Option<&str>,
) -> std::io::Result<()> {
    state.record(Hit {
        method: method.to_string(),
        path: path.to_string(),
        range: range.map(str::to_string),
    });

    let route = state.routes.lock().unwrap().get(path).cloned();

    let Some(route) = route else {
        return write_response(
            socket,
            404,
            &[
                ("Content-Type", "text/plain; charset=utf-8".to_string()),
                ("Content-Length", NOT_FOUND.len().to_string()),
            ],
            Some(NOT_FOUND.to_vec()),
            None,
        )
        .await;
    };

    if let Some(status) = route.status {
        // Gates are answered without a body we care about. `Accept-Ranges` is
        // omitted so the engine does not even try to resume.
        return write_response(
            socket,
            status,
            &[
                ("Content-Type", "text/plain; charset=utf-8".to_string()),
                ("Content-Length", GATED.len().to_string()),
            ],
            Some(GATED.to_vec()),
            None,
        )
        .await;
    }

    let total = route.body.len();

    if total == 0 {
        // No range arithmetic is meaningful, and `bytes 0--1/0` is nonsense.
        return write_response(
            socket,
            200,
            &[
                ("Content-Type", route.content_type.clone()),
                ("Content-Length", "0".to_string()),
            ],
            None,
            None,
        )
        .await;
    }

    let mut headers: Vec<(&str, String)> = vec![("Content-Type", route.content_type.clone())];
    if route.ranges {
        headers.push(("Accept-Ranges", "bytes".to_string()));
    }

    // No range asked for, the route does not support ranges, or the route
    // deliberately ignores them: `200 OK` with everything.
    let wants_range = route.ranges && !route.ignore_range;
    let requested = range.and_then(|r| parse_range(r, total));
    let (mut start, mut end) = match (wants_range, requested) {
        (true, Some((start, end))) => (start, end.unwrap_or(total - 1)),
        _ => (0, total - 1),
    };

    if start >= total {
        return write_response(socket, 416, &[], None, None).await;
    }

    // An end past EOF is clamped, as RFC 9110 requires. An end *before* the
    // start is a malformed range; answering `200 OK` with the whole body is the
    // safe thing for a fixture to do.
    if end < start {
        start = 0;
        end = total - 1;
    } else {
        end = end.min(total - 1);
    }

    // A `HEAD` advertises the same headers with no body, which is what a real
    // origin does and what `capability::probe` expects.
    let slice = route.body[start..=end].to_vec();

    // `Content-Length` describes the bytes on *this* response, not the size of
    // the object. For a `200` they are the same number; for a `206` they differ
    // by exactly the part we are not sending. Advertising the object length on a
    // partial response makes the client wait for bytes that will never arrive
    // and then blame the connection for dying — which reads exactly like a
    // mid-stream network failure and would send us hunting for an off-by-one in
    // our own resume arithmetic.
    headers.push(("Content-Length", slice.len().to_string()));

    let partial = start > 0 || end + 1 != total;
    if partial {
        headers.push(("Content-Range", format!("bytes {start}-{end}/{total}")));
    }

    let body = if method == "HEAD" { None } else { Some(slice) };

    let cut = route.cut_after;
    write_response(socket, if partial { 206 } else { 200 }, &headers, body, cut).await
}

/// Parse `bytes=start-end`, `bytes=start-`, or `bytes=-suffix`.
///
/// `total` is needed to resolve the suffix form, which means "the last `N`
/// bytes" and not "start at `N`" — a fixture that conflates the two would
/// quietly return the wrong bytes for a request the product is entitled to
/// make. `usize` throughout, so the result is directly usable as a slice range.
fn parse_range(value: &str, total: usize) -> Option<(usize, Option<usize>)> {
    let spec = value.strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        // Multi-range is not something we implement; treating it as "no range"
        // is the honest response for a test server.
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    if start.is_empty() {
        let suffix: usize = end.parse().ok()?;
        if suffix == 0 || suffix > total {
            return None;
        }
        return Some((total - suffix, Some(total - 1)));
    }
    let start: usize = start.parse().ok()?;
    let end = if end.is_empty() {
        None
    } else {
        Some(end.parse().ok()?)
    };
    Some((start, end))
}

/// Read one request. Returns `None` if the peer closed before sending one.
///
/// `Option` is folded into the `io::Result` rather than propagated with `?`,
/// which cannot be used on an `Option` here.
async fn read_request(
    socket: &mut TcpStream,
) -> std::io::Result<Option<(String, String, Option<String>)>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];

    // Bounded: a request header block is kilobytes. A fixture that waits for
    // megabytes would hang the suite rather than fail it.
    loop {
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 64 * 1024 {
            break;
        }
    }

    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.split("\r\n");
    let Some(request_line) = lines.next() else {
        return Ok(None);
    };
    if request_line.trim().is_empty() {
        return Ok(None);
    }

    let mut parts = request_line.split_whitespace();
    let Some(method) = parts.next() else {
        return Ok(None);
    };
    let Some(target) = parts.next() else {
        return Ok(None);
    };
    let method = method.to_string();

    // Strip any query string; routes are keyed on the path only.
    let path = target.split('?').next().unwrap_or(target).to_string();

    let mut range = None;
    for line in lines {
        if line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("range") {
                range = Some(value.trim().to_string());
            }
        }
    }

    Ok(Some((method, path, range)))
}

/// Write a response. `cut_after` sends only that many body bytes and then drops
/// the connection, leaving `Content-Length` promising more.
async fn write_response(
    socket: &mut TcpStream,
    status: u16,
    headers: &[(&str, String)],
    body: Option<Vec<u8>>,
    cut_after: Option<usize>,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        206 => "Partial Content",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        416 => "Range Not Satisfiable",
        _ => "Status",
    };

    let mut head = format!("HTTP/1.1 {status} {reason}\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("Connection: close\r\n\r\n");

    socket.write_all(head.as_bytes()).await?;

    if let Some(body) = body {
        match cut_after {
            Some(n) => socket.write_all(&body[..n.min(body.len())]).await?,
            None => socket.write_all(&body).await?,
        }
    }

    socket.flush().await?;
    // Dropping the socket closes the connection.
    Ok(())
}

/// Deterministic pseudo-media of `len` bytes.
///
/// A counter rather than random bytes so a failing assertion shows a readable
/// index instead of noise.
pub fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}
