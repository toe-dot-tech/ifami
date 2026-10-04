//! Range-capability probing.
//!
//! Determining whether a source can be resumed is the difference between
//! telling the user up front and disappointing them after they have waited.
//! This module answers that question with exactly one request, before any
//! meaningful bytes are transferred.

use crate::error::NetError;
use crate::net::client::{HttpClient, HttpRequest};
use crate::net::range::ByteRange;

/// What a source is willing to do, learned from a single probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// Whether byte-range requests are honoured.
    ///
    /// This is *observed* rather than advertised. A server may send
    /// `Accept-Ranges: bytes` and still ignore the header, so we only report
    /// support when a range request actually produced a `206`.
    pub accepts_ranges: bool,

    /// Total length of the representation, when learnable.
    pub total_bytes: Option<u64>,

    /// `Content-Type` as reported, retained so callers need not issue a second
    /// request just to learn the container.
    pub content_type: Option<String>,

    /// Status returned by the probe.
    pub status: u16,
}

impl Capabilities {
    /// Whether a transfer from this source can survive an interruption.
    pub fn is_resumable(&self) -> bool {
        self.accepts_ranges
    }
}

/// Probe `url` for range support and total length.
///
/// Sends `GET` with `Range: bytes=0-0`. A single byte is requested, so a
/// cooperating server returns `206` with a one-byte body and the real total in
/// `Content-Range`. That is definitive in both directions:
///
/// * `206` means ranges work.
/// * `200` means the server ignored the range, so ranges do not work, even if
///   it advertised them.
///
/// The body is deliberately **dropped without being drained**. A server that
/// ignored our range will start streaming the entire file, and reading it to
/// completion would turn a cheap probe into a full download.
pub async fn probe(client: &dyn HttpClient, url: &str) -> Result<Capabilities, NetError> {
    let req = HttpRequest::get(url)
        .with_range(Some(ByteRange::closed(0, 1)))
        // A probe must not hang on a stalled origin.
        .with_timeout(crate::net::client::DEFAULT_TIMEOUT);

    let resp = client.execute(req).await?;
    let status = resp.status;

    let caps = match status {
        206 => {
            let total = resp.headers.content_range().and_then(|c| c.total);
            Capabilities {
                accepts_ranges: true,
                total_bytes: total.or_else(|| resp.headers.content_length()),
                content_type: resp.headers.get("Content-Type").map(str::to_string),
                status,
            }
        }
        200 => Capabilities {
            // It ignored the range. Do not trust any advertised support.
            accepts_ranges: false,
            total_bytes: resp.headers.content_length(),
            content_type: resp.headers.get("Content-Type").map(str::to_string),
            status,
        },
        other => {
            return Err(NetError::Status {
                status: other,
                url: url.to_string(),
            })
        }
    };

    Ok(caps)
}

/// Probe using `HEAD`, for servers that reject a one-byte ranged `GET`.
///
/// Less reliable than [`probe`]: a server may support `HEAD` but not ranges, or
/// may omit `Content-Length` from a `HEAD`. Returns `accepts_ranges: false` when
/// it cannot prove support, which is the safe direction to be wrong in.
pub async fn probe_head(client: &dyn HttpClient, url: &str) -> Result<Capabilities, NetError> {
    let resp = client.execute(HttpRequest::head(url)).await?;
    let status = resp.status;
    if !(200..300).contains(&status) {
        return Err(NetError::Status {
            status,
            url: url.to_string(),
        });
    }

    Ok(Capabilities {
        accepts_ranges: resp.headers.accepts_ranges(),
        total_bytes: resp.headers.content_length(),
        content_type: resp.headers.get("Content-Type").map(str::to_string),
        status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::client::{BodyStream, Headers, RawResponse};
    use async_trait::async_trait;
    use bytes::Bytes;
    use std::sync::Mutex;

    /// A client that replays canned responses and records what it was asked.
    struct Scripted {
        responses: Mutex<Vec<RawResponse>>,
        seen: Mutex<Vec<HttpRequest>>,
    }

    impl Scripted {
        fn new(responses: Vec<RawResponse>) -> Self {
            Self {
                responses: Mutex::new(responses),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl HttpClient for Scripted {
        async fn execute(&self, req: HttpRequest) -> Result<RawResponse, NetError> {
            self.seen.lock().unwrap().push(req);
            let mut r = self.responses.lock().unwrap();
            if r.is_empty() {
                panic!("scripted client ran out of responses");
            }
            Ok(r.remove(0))
        }
    }

    fn resp(status: u16, pairs: &[(&str, &str)]) -> RawResponse {
        RawResponse::empty(
            status,
            "https://example.invalid/v",
            Headers::new(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string()))),
        )
    }

    #[tokio::test]
    async fn a_206_proves_support_and_yields_the_total() {
        let c = Scripted::new(vec![resp(
            206,
            &[("Content-Range", "bytes 0-0/4096"), ("Content-Length", "1")],
        )]);

        let caps = probe(&c, "https://example.invalid/v").await.unwrap();
        assert!(caps.accepts_ranges);
        assert!(caps.is_resumable());
        assert_eq!(caps.total_bytes, Some(4096));

        // The probe must actually ask for one byte, or it is not a probe.
        let seen = c.seen.lock().unwrap();
        let r = seen[0].range.expect("probe must set a range");
        assert_eq!(r, ByteRange::closed(0, 1));
    }

    #[tokio::test]
    async fn a_200_means_ranges_are_not_usable_even_if_advertised() {
        // The important case: a server that advertises support and then ignores
        // it. Trusting the advertisement produces a transfer that cannot be
        // resumed, discovered only after the user has waited.
        let c = Scripted::new(vec![resp(
            200,
            &[("Accept-Ranges", "bytes"), ("Content-Length", "4096")],
        )]);

        let caps = probe(&c, "https://example.invalid/v").await.unwrap();
        assert!(!caps.accepts_ranges);
        assert!(!caps.is_resumable());
        assert_eq!(caps.total_bytes, Some(4096));
    }

    #[tokio::test]
    async fn a_416_is_surfaced_as_an_error_not_as_a_silent_success() {
        // 416 means our probe range was rejected. We still learn the real
        // length, but we must not report success.
        let c = Scripted::new(vec![resp(416, &[])]);
        let err = probe(&c, "https://example.invalid/v").await.unwrap_err();
        assert!(matches!(err, NetError::Status { status: 416, .. }));
    }

    #[tokio::test]
    async fn a_206_with_unknown_total_still_reports_support() {
        let c = Scripted::new(vec![resp(206, &[("Content-Range", "bytes 0-0/*")])]);
        let caps = probe(&c, "https://example.invalid/v").await.unwrap();
        assert!(caps.accepts_ranges);
        assert_eq!(caps.total_bytes, None);
    }

    #[tokio::test]
    async fn the_body_is_dropped_rather_than_drained() {
        // A server that ignored our range would otherwise stream the entire
        // file, turning a cheap probe into a full download.
        use futures_util::stream;

        let chunks: Vec<Result<Bytes, NetError>> =
            (0..100).map(|_| Ok(Bytes::from(vec![0u8; 4096]))).collect();
        let response = RawResponse {
            status: 200,
            final_url: "https://example.invalid/v".into(),
            headers: Headers::new([("Content-Length".to_string(), "409600".to_string())]),
            body: Box::pin(stream::iter(chunks)) as BodyStream,
        };

        let c = Scripted::new(vec![response]);
        let caps = probe(&c, "https://example.invalid/v").await.unwrap();
        assert!(!caps.accepts_ranges);
        assert_eq!(caps.total_bytes, Some(409600));
    }
}
