//! The default [`HttpClient`], backed by `reqwest`.
//!
//! Available behind the `network` feature. Disable that feature when embedding
//! `ifami-core` in a host that supplies its own transport, and the core
//! inherits without pulling in an HTTP stack.
//!
//! # What this transport deliberately does not do
//!
//! * **No client impersonation.** `curl_cffi`-style TLS/HTTP2 fingerprint
//!   spoofing is absent by design. See ADR-0005.
//! * **No cookie jar.** Ifami does not maintain session state across requests,
//!   and does not accept cookies from a server it did not ask for.
//! * **No JavaScript execution.** Not because `reqwest` cannot, but because it
//!   cannot, and that is the correct capability set for this project.
//! * **No credential forwarding across hosts.** `Authorization` headers set by
//!   the caller for the user's own content are only sent to the host they were
//!   given for; see [`ReqwestClient::execute`].

use async_trait::async_trait;
use bytes::Bytes;
use futures_core::Stream;

use crate::error::NetError;
use crate::net::client::{
    BodyStream, Headers, HttpClient, HttpRequest, Method, RawResponse, RequestBody,
};

/// Builder for [`ReqwestClient`].
pub struct ReqwestClientBuilder {
    inner: reqwest::ClientBuilder,
    user_agent: String,
}

impl Default for ReqwestClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ReqwestClientBuilder {
    /// A builder with Ifami's default configuration.
    pub fn new() -> Self {
        let user_agent = crate::USER_AGENT.to_string();
        let inner = reqwest::Client::builder()
            // We identify honestly and do not claim to be any browser.
            .user_agent(user_agent.clone())
            // Redirects are followed up to a limit, then reported.
            .redirect(reqwest::redirect::Policy::limited(
                crate::net::client::MAX_REDIRECTS,
            ))
            // Compression stays OFF, and the `gzip`/`brotli`/`deflate` features are
            // deliberately not enabled in `Cargo.toml`. This is a correctness
            // decision, not an oversight: with content coding on, `Content-Length`
            // counts *compressed* bytes, so the length we record as "total" and the
            // offsets we later send as `Range: bytes=N-` refer to two different
            // coordinate systems. Every range-based resume then lands in the wrong
            // place and the file is silently corrupt.
            .connect_timeout(std::time::Duration::from_secs(15))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .pool_max_idle_per_host(8);
        // `https_only` stays off: we already refuse every non-http(s) scheme in
        // `execute` below, and turning it on would break the `127.0.0.1`
        // fixture server the integration tests run against.

        Self { inner, user_agent }
    }

    /// Override the user-agent.
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = ua.into();
        self.inner = self.inner.user_agent(self.user_agent.clone());
        self
    }

    /// Apply transport options directly.
    pub fn configure(
        mut self,
        f: impl FnOnce(&mut reqwest::ClientBuilder) -> &mut reqwest::ClientBuilder,
    ) -> Self {
        f(&mut self.inner);
        self
    }

    /// Build the client.
    pub fn build(self) -> Result<ReqwestClient, NetError> {
        let client = self.inner.build().map_err(|e| NetError::Tls {
            url: String::new(),
            reason: e.to_string(),
        })?;
        Ok(ReqwestClient {
            client,
            user_agent: self.user_agent,
        })
    }
}

/// The default [`HttpClient`].
#[derive(Debug, Clone)]
pub struct ReqwestClient {
    client: reqwest::Client,
    user_agent: String,
}

impl ReqwestClient {
    /// Build with default configuration.
    pub fn new() -> Result<Self, NetError> {
        ReqwestClientBuilder::new().build()
    }

    /// Access the underlying `reqwest` client, for advanced configuration.
    pub fn reqwest(&self) -> &reqwest::Client {
        &self.client
    }
}

#[async_trait]
impl HttpClient for ReqwestClient {
    async fn execute(&self, req: HttpRequest) -> Result<RawResponse, NetError> {
        // Reject schemes we will never fetch, before a socket is opened.
        let parsed = url::Url::parse(&req.url).map_err(|e| NetError::InvalidUrl {
            raw: req.url.clone(),
            reason: e.to_string(),
        })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(NetError::InvalidUrl {
                raw: req.url.clone(),
                reason: format!("scheme `{}` is not http or https", parsed.scheme()),
            });
        }

        let method = match req.method {
            Method::Get => reqwest::Method::GET,
            Method::Head => reqwest::Method::HEAD,
            Method::Post => reqwest::Method::POST,
        };

        let mut builder = self
            .client
            .request(method, &req.url)
            .timeout(req.effective_timeout());

        if let Some(range) = req.range {
            builder = builder.header(reqwest::header::RANGE, range.to_header_value());
        }

        // Declared before the caller's own headers, so an explicit `Content-Type`
        // or `Content-Length` still wins: a caller that knows what it is sending
        // is allowed to say so.
        if let Some(body) = &req.body {
            if !body.is_empty() {
                builder = builder
                    .header(reqwest::header::CONTENT_LENGTH, body.len())
                    .body(request_body(body));
            }
        }

        for (name, value) in &req.headers {
            builder = builder.header(name, value);
        }

        let response = builder
            .send()
            .await
            .map_err(|e| map_reqwest_error(e, &req.url))?;

        let status = response.status().as_u16();
        let final_url = response.url().to_string();

        let headers = Headers::new(response.headers().iter().map(|(k, v)| {
            (
                k.as_str().to_string(),
                v.to_str().unwrap_or_default().to_string(),
            )
        }));

        let body: BodyStream = if req.method == Method::Head {
            Box::pin(futures_util::stream::empty())
        } else {
            Box::pin(response_stream(response))
        };

        Ok(RawResponse {
            status,
            final_url,
            headers,
            body,
        })
    }

    fn user_agent(&self) -> &str {
        &self.user_agent
    }
}

/// Adapt `reqwest`'s byte stream to ours, mapping mid-transfer failures into
/// [`NetError::Body`] so the URL is preserved in the message.
fn response_stream(
    response: reqwest::Response,
) -> impl Stream<Item = Result<Bytes, NetError>> + Send {
    let url = response.url().to_string();
    response.bytes_stream().map(move |chunk| {
        chunk.map_err(|e| NetError::Body {
            url: url.clone(),
            reason: e.to_string(),
        })
    })
}

/// Turn a [`RequestBody`] into something `reqwest` will stream out.
///
/// The repeating arm holds one buffer and a remaining count, and yields slices
/// of it. `Bytes` is a refcount, so cloning the chunk into each `unfold` state
/// costs an atomic increment rather than a copy -- and no `Arc` is needed
/// anywhere, because the state is owned by the stream itself.
///
/// The final yield is a partial slice when `total` is not a whole number of
/// chunks, so the body sends exactly the number of bytes it promised.
fn request_body(body: &RequestBody) -> reqwest::Body {
    match body {
        RequestBody::Bytes(b) => reqwest::Body::from(b.clone()),
        RequestBody::Repeated { chunk, total } => {
            let state = Some((chunk.clone(), *total));
            let stream = futures_util::stream::unfold(state, |state| async move {
                let (chunk, remaining) = state?;
                if remaining == 0 {
                    return None;
                }
                let take = (chunk.len() as u64).min(remaining) as usize;
                let next = Some((chunk.clone(), remaining - take as u64));
                Some((Ok::<Bytes, std::io::Error>(chunk.slice(0..take)), next))
            });
            reqwest::Body::wrap_stream(stream)
        }
    }
}

/// Translate `reqwest`'s error taxonomy into ours.
fn map_reqwest_error(e: reqwest::Error, url: &str) -> NetError {
    if e.is_timeout() {
        NetError::Timeout {
            url: url.to_string(),
        }
    } else if e.is_redirect() {
        NetError::TooManyRedirects {
            url: url.to_string(),
            limit: crate::net::client::MAX_REDIRECTS,
        }
    } else if e.is_builder() {
        NetError::InvalidUrl {
            raw: url.to_string(),
            reason: e.to_string(),
        }
    } else if e.is_connect() {
        // `reqwest` does not separate DNS from connection failure.
        NetError::Dns {
            url: url.to_string(),
        }
    } else {
        // Everything else — `is_body`, `is_decode`, and whatever `reqwest`
        // grows next year — is reported as a body error.
        //
        // There used to be an `is_body() || is_decode()` branch above this one
        // that built exactly this same value, which made it dead code wearing
        // the costume of a distinction. The catch-all is deliberate: every
        // unclassified failure here happens with a request in flight, and
        // "the transfer broke" is both true and the most useful thing we can
        // say about it. A `reqwest` variant we have never seen of should not
        // be reported as a redirect or a timeout just because it sorted after
        // the branches we know.
        NetError::Body {
            url: url.to_string(),
            reason: e.to_string(),
        }
    }
}

use futures_util::StreamExt as _;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn non_http_schemes_are_refused_before_any_socket_is_opened() {
        // `file:` would otherwise read local files, and `smb:`/`ftp:` reach
        // internal services. Refusing at the URL layer is the only place this
        // can be enforced reliably.
        let c = ReqwestClient::new().unwrap();
        for bad in [
            "file:///C:/Windows/win.ini",
            "smb://internal/share/secret",
            "ftp://example.invalid/x",
            "data:text/plain,hello",
        ] {
            let err = c.execute(HttpRequest::get(bad)).await.unwrap_err();
            assert!(
                matches!(err, NetError::InvalidUrl { .. }),
                "{bad} produced {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_malformed_url_is_reported_as_invalid() {
        let c = ReqwestClient::new().unwrap();
        let err = c
            .execute(HttpRequest::get(":://not a url"))
            .await
            .unwrap_err();
        assert!(matches!(err, NetError::InvalidUrl { .. }));
    }

    #[test]
    fn the_default_user_agent_names_ifami_and_no_browser() {
        let c = ReqwestClient::new().unwrap();
        let ua = c.user_agent();
        assert!(ua.starts_with("ifami/"), "{ua}");
        assert!(!ua.contains("Mozilla"), "{ua}");
        assert!(!ua.contains("Safari"), "{ua}");
    }
}
