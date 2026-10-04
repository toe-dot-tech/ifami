//! The injectable transport seam.
//!
//! Every network operation in the engine goes through [`HttpClient`]. That is
//! what makes the engine testable: unit tests drive a scripted client with
//! byte-exact responses and no sockets, and integration tests drive the real
//! client against a `127.0.0.1` fixture server. Neither touches the public
//! network, and CI asserts that.
//!
//! The trait is intentionally small. It is a transport, not a browser: no
//! cookie jar, no redirect policy beyond a limit, no TLS impersonation, no
//! JavaScript execution. Those absences are deliberate and are load-bearing for
//! the scope boundary in `docs/SCOPE.md`.

use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures_core::Stream;

use crate::error::NetError;
use crate::net::range::{ByteRange, ContentRange};

/// HTTP methods Ifami issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Method {
    /// Fetch the whole representation.
    #[default]
    Get,
    /// Fetch headers only. Used for cheap capability probing where the server
    /// does not support range requests.
    Head,
    /// Send a body to the origin.
    ///
    /// Present for exactly one caller -- the upload half of the speed test. It
    /// is not a general-purpose write verb: nothing in the engine ever posts
    /// anywhere except a speed-test target the backend chose, so if you are
    /// reaching for this to talk to an API, that is a different product.
    Post,
}

impl Method {
    /// The method name as it appears on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
        }
    }
}

/// A request payload.
#[derive(Debug, Clone)]
pub enum RequestBody {
    /// A fixed buffer, sent as-is.
    Bytes(Bytes),
    /// `total` bytes formed by repeating `chunk`, generated as it is sent.
    ///
    /// Not a convenience. An upload measurement pushes tens of megabytes through
    /// a single request, and a `Bytes` body of the full size would allocate the
    /// entire payload up front -- so the thing being measured would be the
    /// allocator rather than the network, and the allocation itself would be
    /// charged to the user's data plan. A repeating body is one buffer's worth
    /// of memory for a body of any size.
    Repeated {
        /// The unit that is repeated. Never empty.
        chunk: Bytes,
        /// How many bytes to send in total.
        total: u64,
    },
}

impl RequestBody {
    /// Build a repeating body, rounded down to whole chunks.
    ///
    /// A `total` that is not a multiple of the chunk length still produces a
    /// body of exactly `total` bytes: the final piece is a partial slice. Clamping
    /// instead would quietly send less than the caller asked for and make the
    /// reported throughput wrong by the difference.
    ///
    /// An empty `chunk` would yield an endless body of nothing, so it is refused
    /// rather than looped on.
    pub fn repeated(chunk: Bytes, total: u64) -> Option<Self> {
        (!chunk.is_empty()).then_some(Self::Repeated { chunk, total })
    }

    /// How many bytes this body will put on the wire.
    ///
    /// Exactly `total` for a repeated body, never rounded up to a whole number of
    /// chunks: the caller budgeted `total` bytes and is going to be charged for
    /// `total` bytes, and a body that sent more than it was asked for would make
    /// the ceiling meaningless.
    pub fn len(&self) -> u64 {
        match self {
            Self::Bytes(b) => b.len() as u64,
            Self::Repeated { total, .. } => *total,
        }
    }

    /// Whether this body sends nothing.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Default per-request deadline.
///
/// Deliberately generous: a large file over a slow connection legitimately takes
/// a long time, and this bounds the *stall*, not the total transfer. Idle
/// timeouts handle stalls in the transport.
pub const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Maximum redirects followed before giving up.
pub const MAX_REDIRECTS: usize = 10;

/// A request to be issued by an [`HttpClient`].
#[derive(Debug, Clone)]
pub struct HttpRequest {
    /// Absolute URL.
    pub url: String,
    /// HTTP method.
    pub method: Method,
    /// Additional request headers, applied after our defaults.
    pub headers: Vec<(String, String)>,
    /// Byte range to request. `None` requests the whole representation.
    pub range: Option<ByteRange>,
    /// Per-request deadline.
    pub timeout: Option<std::time::Duration>,
    /// Payload to send, for methods that carry one.
    ///
    /// `None` for every method except [`Method::Post`], which is a request
    /// without a body by accident rather than by design -- so the transport
    /// treats it as an empty body rather than inventing one.
    pub body: Option<RequestBody>,
}

impl HttpRequest {
    /// A plain GET.
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            method: Method::Get,
            headers: Vec::new(),
            range: None,
            timeout: None,
            body: None,
        }
    }

    /// A HEAD request.
    pub fn head(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            method: Method::Head,
            headers: Vec::new(),
            range: None,
            timeout: None,
            body: None,
        }
    }

    /// A POST carrying `body`.
    ///
    /// `range` is left unset deliberately: a ranged POST has no meaning here,
    /// and letting one through would be a request whose headers and body
    /// disagree about what is being sent.
    pub fn post(url: impl Into<String>, body: RequestBody) -> Self {
        Self {
            url: url.into(),
            method: Method::Post,
            headers: Vec::new(),
            range: None,
            timeout: None,
            body: Some(body),
        }
    }

    /// Set the range to request.
    pub fn with_range(mut self, range: Option<ByteRange>) -> Self {
        self.range = range;
        self
    }

    /// Set the deadline.
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Add a request header.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Replace the body.
    pub fn with_body(mut self, body: Option<RequestBody>) -> Self {
        self.body = body;
        self
    }

    /// How many bytes this request will put on the wire, ignoring the headers.
    pub fn body_len(&self) -> u64 {
        self.body.as_ref().map_or(0, RequestBody::len)
    }

    /// The effective deadline.
    pub fn effective_timeout(&self) -> std::time::Duration {
        self.timeout.unwrap_or(DEFAULT_TIMEOUT)
    }
}

/// Case-insensitive header collection.
///
/// Hand-rolled because the engine's header needs are small and we would rather
/// not carry a dependency for a case-insensitive `Vec` lookup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    /// Build from name/value pairs.
    pub fn new(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        Self(pairs.into_iter().collect())
    }

    /// First value for `name`, case-insensitively.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// All values for `name`.
    pub fn get_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.0
            .iter()
            .filter(move |(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// All headers as pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Whether the response advertises byte-range support.
    ///
    /// Absent `Accept-Ranges` is treated as "not supported", per RFC 9110. This
    /// is the conservative reading, and it matters: assuming support we do not
    /// have produces a transfer that cannot be resumed, discovered only after
    /// the user has waited.
    pub fn accepts_ranges(&self) -> bool {
        self.get_all("Accept-Ranges")
            .any(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("bytes")))
    }

    /// `Content-Length`, when present and valid.
    pub fn content_length(&self) -> Option<u64> {
        self.get("Content-Length")?.trim().parse().ok()
    }

    /// Parsed `Content-Range`, when present and valid.
    pub fn content_range(&self) -> Option<ContentRange> {
        let raw = self.get("Content-Range")?;
        crate::net::range::parse_content_range(raw).ok()
    }

    /// Raw `Content-Range` value, including any value we failed to parse.
    pub fn content_range_raw(&self) -> Option<&str> {
        self.get("Content-Range")
    }
}

/// A stream of response body chunks.
pub type BodyStream = Pin<Box<dyn Stream<Item = Result<Bytes, NetError>> + Send>>;

/// An HTTP response as returned by an [`HttpClient`].
///
/// Body errors are carried inside the stream rather than returned eagerly, so
/// a mid-transfer failure surfaces at the point of writing rather than being
/// lost.
pub struct RawResponse {
    /// Status code.
    pub status: u16,
    /// URL after redirects.
    pub final_url: String,
    /// Response headers, with duplicates preserved.
    pub headers: Headers,
    /// Body stream. Empty for `HEAD`.
    pub body: BodyStream,
}

impl std::fmt::Debug for RawResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The body is a stream and cannot be usefully printed.
        f.debug_struct("RawResponse")
            .field("status", &self.status)
            .field("final_url", &self.final_url)
            .field("headers", &self.headers)
            .field("body", &"<stream>")
            .finish()
    }
}

impl RawResponse {
    /// Whether the status is 2xx.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Total length of the whole representation, preferring `Content-Range`'s
    /// total over `Content-Length`.
    ///
    /// On a `206`, `Content-Length` is the length of *this chunk*, not of the
    /// object. Using it as a total is a classic source of "transfer complete at
    /// 30%" bugs, so the `Content-Range` total wins when both are present.
    pub fn total_bytes(&self) -> Option<u64> {
        self.headers
            .content_range()
            .and_then(|c| c.total)
            .or_else(|| self.headers.content_length())
    }

    /// Build a response with no body.
    pub fn empty(status: u16, final_url: impl Into<String>, headers: Headers) -> Self {
        Self {
            status,
            final_url: final_url.into(),
            headers,
            body: Box::pin(futures_util::stream::empty()),
        }
    }

    /// Build a response whose body is `body`, delivered as a single chunk.
    ///
    /// Exists for tests that need a `RawResponse` carrying a document. The
    /// production client never constructs one of these, so a unit test using it
    /// cannot accidentally model something the real transport cannot do.
    #[cfg(test)]
    pub fn full(
        status: u16,
        body: impl Into<Bytes>,
        final_url: impl Into<String>,
        headers: Headers,
    ) -> Self {
        let chunk = body.into();
        Self {
            status,
            final_url: final_url.into(),
            headers,
            body: Box::pin(futures_util::stream::once(async move {
                Ok::<Bytes, NetError>(chunk)
            })),
        }
    }
}

/// Why reading a whole body failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CollectError {
    /// The body exceeded the caller's limit.
    #[error("response body exceeded the {limit}-byte limit")]
    TooLarge {
        /// The limit that was enforced.
        limit: usize,
    },

    /// The transport failed partway through.
    #[error(transparent)]
    Net(#[from] NetError),
}

/// Read a response body into memory, refusing to exceed `limit`.
///
/// The limit is enforced while streaming, not after collecting, so a hostile
/// or broken server cannot make the engine allocate without bound. Manifests
/// are kilobytes; a document that exceeds a few megabytes is not a document.
pub async fn collect_limited(response: RawResponse, limit: usize) -> Result<Vec<u8>, CollectError> {
    use futures_util::StreamExt;

    let RawResponse {
        status,
        final_url,
        headers,
        mut body,
    } = response;

    if !(200..300).contains(&status) {
        return Err(CollectError::Net(NetError::Status {
            status,
            url: final_url,
        }));
    }

    // Trust Content-Length enough to fail fast, but still enforce the ceiling
    // while reading, because Content-Length is a claim by the server.
    if let Some(len) = headers.content_length() {
        if len > limit as u64 {
            return Err(CollectError::TooLarge { limit });
        }
    }

    let mut out = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        if out.len() + chunk.len() > limit {
            return Err(CollectError::TooLarge { limit });
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// A transport that can issue Ifami's requests.
///
/// Implementors must not add behaviour beyond transport: no cookie jar, no
/// credential reuse across hosts, no client impersonation. If you find yourself
/// wanting any of those in an implementation, the answer is no, and the reason
/// is in `docs/SCOPE.md`.
#[async_trait]
pub trait HttpClient: Send + Sync {
    /// Issue `req` and return the response.
    async fn execute(&self, req: HttpRequest) -> Result<RawResponse, NetError>;

    /// User-agent string this client identifies as.
    ///
    /// Ifami names itself honestly and does not claim to be a browser. See
    /// ADR-0005.
    fn user_agent(&self) -> &str {
        crate::USER_AGENT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> Headers {
        Headers::new(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())))
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let h = headers(&[("Content-Type", "video/mp4")]);
        assert_eq!(h.get("content-type"), Some("video/mp4"));
        assert_eq!(h.get("CONTENT-TYPE"), Some("video/mp4"));
        assert_eq!(h.get("missing"), None);
    }

    #[test]
    fn absent_accept_ranges_means_unsupported() {
        // RFC 9110: an absent header is not a claim of support. Assuming
        // support we do not have means discovering the transfer is not
        // resumable only after the user has waited.
        assert!(!headers(&[]).accepts_ranges());
        assert!(!headers(&[("Accept-Ranges", "none")]).accepts_ranges());
        assert!(headers(&[("Accept-Ranges", "bytes")]).accepts_ranges());
        assert!(headers(&[("Accept-Ranges", "none, bytes")]).accepts_ranges());
    }

    #[test]
    fn total_bytes_prefers_content_range_over_content_length() {
        // On a 206, Content-Length is the chunk length, not the object length.
        let r = RawResponse::empty(
            206,
            "https://example.invalid/v",
            headers(&[
                ("Content-Length", "700"),
                ("Content-Range", "bytes 300-999/1000"),
            ]),
        );
        assert_eq!(r.total_bytes(), Some(1000));
    }

    #[test]
    fn total_bytes_falls_back_to_content_length_on_200() {
        let r = RawResponse::empty(
            200,
            "https://example.invalid/v",
            headers(&[("Content-Length", "1000")]),
        );
        assert_eq!(r.total_bytes(), Some(1000));
    }

    #[test]
    fn raw_content_range_is_preserved_even_when_unparseable() {
        // decide_resume needs the raw value so it can distinguish "absent" from
        // "present but nonsense"; both lead to a restart, but for different
        // reasons worth keeping apart in a log.
        let h = headers(&[("Content-Range", "not-a-range")]);
        assert!(h.content_range().is_none());
        assert_eq!(h.content_range_raw(), Some("not-a-range"));
    }

    #[tokio::test]
    async fn collect_limited_rejects_oversized_content_length_without_reading() {
        let resp = RawResponse::empty(
            200,
            "https://example.invalid/big",
            headers(&[("Content-Length", "99999999")]),
        );
        assert!(matches!(
            collect_limited(resp, 1024).await,
            Err(CollectError::TooLarge { limit: 1024 })
        ));
    }

    #[tokio::test]
    async fn collect_limited_passes_through_a_small_body() {
        use futures_util::stream;

        let resp = RawResponse {
            status: 200,
            final_url: "https://example.invalid/small".into(),
            headers: headers(&[("Content-Length", "5")]),
            body: Box::pin(stream::iter(vec![
                Ok(Bytes::from_static(b"hello")),
                Ok(Bytes::new()),
            ])),
        };
        assert_eq!(collect_limited(resp, 1024).await.unwrap(), b"hello");
    }

    #[tokio::test]
    async fn collect_limited_enforces_the_ceiling_even_without_content_length() {
        // A lying or chunked server must not be able to make us allocate.
        use futures_util::stream;

        let chunks: Vec<Result<Bytes, NetError>> = (0..10)
            .map(|_| Ok(Bytes::from_static(&[b'x'; 100])))
            .collect();
        let resp = RawResponse {
            status: 200,
            final_url: "https://example.invalid/stream".into(),
            headers: Headers::default(),
            body: Box::pin(stream::iter(chunks)),
        };
        assert!(matches!(
            collect_limited(resp, 512).await,
            Err(CollectError::TooLarge { limit: 512 })
        ));
    }

    #[tokio::test]
    async fn collect_limited_maps_a_non_2xx_to_a_net_error() {
        let resp = RawResponse::empty(404, "https://example.invalid/nope", Headers::default());
        assert!(matches!(
            collect_limited(resp, 1024).await,
            Err(CollectError::Net(NetError::Status { status: 404, .. }))
        ));
    }

    #[test]
    fn request_builder_defaults_are_sane() {
        let r = HttpRequest::get("https://example.invalid/x");
        assert_eq!(r.method, Method::Get);
        assert_eq!(r.range, None);
        assert_eq!(r.effective_timeout(), DEFAULT_TIMEOUT);
        assert_eq!(
            r.with_range(Some(ByteRange::from_offset(5)))
                .range
                .unwrap()
                .start,
            5
        );
    }

    #[test]
    fn the_user_agent_does_not_impersonate_a_browser() {
        // Asserted rather than merely documented: an "ifami/..." UA is a
        // deliberate claim, and it is easy to regress.
        struct Probe;
        #[async_trait]
        impl HttpClient for Probe {
            async fn execute(&self, _: HttpRequest) -> Result<RawResponse, NetError> {
                unimplemented!("not called")
            }
        }
        let ua = Probe.user_agent();
        assert!(ua.starts_with("ifami/"));
        assert!(!ua.contains("Chrome"));
        assert!(!ua.contains("Safari"));
        assert!(!ua.contains("Mozilla"));
    }
}
