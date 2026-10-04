//! Error taxonomy for the Ifami engine.
//!
//! Every failure mode reachable from a user-supplied URL lands in exactly one
//! of the three domain enums ([`NetError`], [`ResolveError`], [`TransferError`])
//! and is then wrapped in [`Error`]. This split is deliberate: callers need to
//! distinguish "the network misbehaved" (retryable) from "this source is out of
//! scope" (a final answer, never retried), and collapsing them into one enum
//! is how download managers end up retrying `AuthRequired` forever.
//!
//! See `docs/SCOPE.md` for the normative list of what Ifami declines to do.

use std::path::PathBuf;

/// Convenience alias for engine results.
pub type Result<T> = std::result::Result<T, Error>;

/// Top-level engine error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The transport failed.
    #[error("network error: {0}")]
    Net(#[from] NetError),

    /// A URL could not be turned into a set of downloadable formats.
    #[error("could not resolve media: {0}")]
    Resolve(#[from] ResolveError),

    /// A transfer failed after formats were resolved.
    #[error("transfer failed: {0}")]
    Transfer(#[from] TransferError),

    /// A filesystem operation failed. Carries the path so the message is
    /// actionable without a backtrace.
    #[error("i/o error at {path}: {source}")]
    Io {
        /// Path being operated on.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The on-disk queue could not be read or written.
    #[error("queue store is corrupt or unreadable: {0}")]
    Store(String),

    /// The caller supplied an argument the engine cannot honour.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}

impl Error {
    /// Attach a path to an [`std::io::Error`].
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    /// Whether retrying the operation could plausibly succeed.
    ///
    /// Resolution errors are never retryable: [`ResolveError::AuthRequired`] and
    /// [`ResolveError::DrmProtected`] are deliberate, permanent answers, and
    /// retrying them is both futile and, for a user, misleading.
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Net(e) => e.is_retryable(),
            Error::Transfer(e) => e.is_retryable(),
            Error::Resolve(_) | Error::Store(_) | Error::InvalidArgument(_) | Error::Io { .. } => {
                false
            }
        }
    }
}

/// Transport-level failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NetError {
    /// A non-success HTTP status was returned.
    #[error("http status {status} from {url}")]
    Status {
        /// HTTP status code.
        status: u16,
        /// URL that produced it.
        url: String,
    },

    /// The request exceeded its deadline.
    #[error("request to {url} timed out")]
    Timeout {
        /// URL that timed out.
        url: String,
    },

    /// Name resolution failed.
    #[error("could not resolve host for {url}")]
    Dns {
        /// URL whose host could not be resolved.
        url: String,
    },

    /// TLS negotiation or certificate validation failed.
    #[error("tls error for {url}: {reason}")]
    Tls {
        /// URL that failed.
        url: String,
        /// Human-readable cause, supplied by the transport.
        reason: String,
    },

    /// The response body ended early or was otherwise unreadable.
    #[error("response body from {url} failed: {reason}")]
    Body {
        /// URL whose body failed.
        url: String,
        /// Human-readable cause.
        reason: String,
    },

    /// The server sent more redirects than we are willing to follow.
    #[error("too many redirects starting at {url} (limit {limit})")]
    TooManyRedirects {
        /// Original URL.
        url: String,
        /// Configured limit.
        limit: usize,
    },

    /// The URL could not be parsed.
    #[error("invalid url {raw}: {reason}")]
    InvalidUrl {
        /// The unparseable input.
        raw: String,
        /// Human-readable cause.
        reason: String,
    },
}

impl NetError {
    /// Whether retrying the same request could plausibly succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            NetError::Status { status, .. } => *status == 429 || *status >= 500,
            NetError::Timeout { .. } | NetError::Dns { .. } | NetError::Tls { .. } => true,
            NetError::Body { .. } => true,
            NetError::TooManyRedirects { .. } | NetError::InvalidUrl { .. } => false,
        }
    }
}

/// Failures produced while turning a URL into downloadable formats.
///
/// [`ResolveError::AuthRequired`] and [`ResolveError::DrmProtected`] are
/// load-bearing: they are the mechanism by which the scope boundary in
/// `docs/SCOPE.md` is enforced in code rather than merely documented.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ResolveError {
    /// The source gates access and Ifami will not bypass it.
    ///
    /// This is a **correct final answer**, not a transient failure. It is
    /// returned when a source requires authentication, a subscription, a
    /// paywall, a geo-restriction, or an anti-bot attestation token to retrieve
    /// the media. See `docs/SCOPE.md` and `docs/adr/0005-scope-boundary.md`.
    #[error(
        "this source requires authentication, a membership, or access-control bypass, \
         which is out of scope for ifami (see docs/SCOPE.md)"
    )]
    AuthRequired,

    /// The media is DRM-protected and Ifami will not decrypt it.
    ///
    /// Returned when a manifest declares an encrypted content-protection
    /// scheme. Ifami does not ship a CDM and never will.
    #[error("this media is DRM-protected; decryption is out of scope for ifami")]
    DrmProtected,

    /// The URL resolved cleanly but contained nothing downloadable.
    #[error("no downloadable media found at {url}")]
    NoMedia {
        /// URL that contained no media.
        url: String,
    },

    /// The URL scheme is not one Ifami handles.
    #[error("unsupported url scheme `{scheme}` in {url}")]
    UnsupportedScheme {
        /// The scheme encountered.
        scheme: String,
        /// The full URL.
        url: String,
    },

    /// A document exceeded the parser's size limit.
    ///
    /// Bounded on purpose: a hostile or broken server must not be able to make
    /// the engine allocate without limit.
    #[error("document from {url} exceeded the {limit}-byte parse limit")]
    ResponseTooLarge {
        /// URL being parsed.
        url: String,
        /// The configured limit.
        limit: usize,
    },

    /// A manifest or page was syntactically invalid.
    #[error("malformed document at {url}: {reason}")]
    Malformed {
        /// URL being parsed.
        url: String,
        /// Human-readable cause.
        reason: String,
    },

    /// The manifest uses a streaming/live configuration Ifami does not support.
    #[error("unsupported manifest feature at {url}: {feature}")]
    UnsupportedFeature {
        /// URL being parsed.
        url: String,
        /// The unsupported feature, named for the user.
        feature: String,
    },
}

/// Failures during the transfer phase.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransferError {
    /// The server does not advertise range support, so the transfer cannot be
    /// resumed after an interruption.
    ///
    /// Surfaced at resolve time when detectable, so the user learns it before
    /// waiting, rather than after.
    #[error("this source does not support range requests, so it cannot be resumed")]
    NotResumable,

    /// The server ignored our `Range` header and returned a full body.
    ///
    /// We discard the partial file rather than appending a full copy, which
    /// would silently produce a corrupt output. This is the most common
    /// corruption bug in download managers.
    #[error("server ignored the range request and returned a full body; partial data discarded")]
    RangeIgnored,

    /// The remote object changed between two requests.
    #[error(
        "remote object changed mid-transfer: we had {expected} bytes, server reports {actual}"
    )]
    RemoteChanged {
        /// Bytes believed to be present before the request.
        expected: u64,
        /// Total the server now claims.
        actual: u64,
    },

    /// The transfer ended before reaching the expected length.
    #[error("transfer incomplete: wrote {written} bytes of an expected {expected}")]
    Incomplete {
        /// Bytes successfully written.
        written: u64,
        /// Bytes expected in total.
        expected: u64,
    },

    /// A successful status arrived but the `Content-Range` was unusable.
    #[error("malformed or unusable Content-Range header: {raw:?}")]
    BadContentRange {
        /// The header exactly as received.
        raw: String,
    },

    /// The transfer was paused or cancelled by the user.
    ///
    /// Not a failure. Carried as an error so that `?` propagates cancellation
    /// out of deep call stacks without every layer inspecting a flag.
    #[error("transfer paused")]
    Paused,

    /// A non-success status arrived partway through the body.
    #[error("http status {status} received mid-transfer")]
    MidTransfer {
        /// The status received.
        status: u16,
    },

    /// The destination path could not be prepared.
    #[error("cannot write to destination: {0}")]
    Destination(String),

    /// Referenced a segment index that was never fetched, so the output would
    /// be incomplete but structurally valid. Treated as a hard failure rather
    /// than a warning because a silently short file is worse than an error.
    #[error("segment {index} is missing from a fragmented transfer")]
    MissingSegment {
        /// Index of the missing segment.
        index: u32,
    },
}

impl TransferError {
    /// Whether resuming the transfer could plausibly succeed.
    ///
    /// [`TransferError::Paused`] is retryable because resuming is precisely the
    /// point of the pause. [`TransferError::MissingSegment`] is retryable
    /// because the plan is re-walked and only the absent segment is fetched.
    pub fn is_retryable(&self) -> bool {
        match self {
            TransferError::NotResumable
            | TransferError::RangeIgnored
            | TransferError::RemoteChanged { .. }
            | TransferError::Destination(_) => false,

            TransferError::Paused
            | TransferError::Incomplete { .. }
            | TransferError::BadContentRange { .. }
            | TransferError::MidTransfer { .. }
            | TransferError::MissingSegment { .. } => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_errors_are_never_retryable() {
        // This is the invariant that stops a client from hammering a source that
        // we have deliberately declined to access.
        for err in [
            ResolveError::AuthRequired,
            ResolveError::DrmProtected,
            ResolveError::NoMedia {
                url: "https://example.invalid".into(),
            },
        ] {
            let wrapped = Error::Resolve(err);
            assert!(
                !wrapped.is_retryable(),
                "resolve errors must be final: {wrapped}"
            );
        }
    }

    #[test]
    fn server_side_statuses_are_retryable_but_4xx_is_not() {
        assert!(NetError::Status {
            status: 503,
            url: String::new()
        }
        .is_retryable());
        assert!(NetError::Status {
            status: 429,
            url: String::new()
        }
        .is_retryable());
        assert!(!NetError::Status {
            status: 404,
            url: String::new()
        }
        .is_retryable());
        assert!(!NetError::InvalidUrl {
            raw: ":://".into(),
            reason: "relative URL without a base".into()
        }
        .is_retryable());
    }

    #[test]
    fn pause_is_retryable_but_corruption_is_not() {
        assert!(TransferError::Paused.is_retryable());
        assert!(!TransferError::RangeIgnored.is_retryable());
        assert!(!TransferError::RemoteChanged {
            expected: 1,
            actual: 2
        }
        .is_retryable());
    }
}
