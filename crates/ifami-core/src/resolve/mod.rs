//! Turning a URL into something downloadable.
//!
//! A [`Resolver`] claims a URL and turns it into a [`Media`] containing one or
//! more [`Format`]s. Resolvers are tried in registration order, first match
//! wins, so more specific matchers must be registered first.
//!
//! # Scope enforcement
//!
//! Two [`ResolveError`] variants are load-bearing and are the mechanism by
//! which `docs/SCOPE.md` is enforced in code rather than merely documented:
//! * [`ResolveError::AuthRequired`] — the source gates access and we decline.
//! * [`ResolveError::DrmProtected`] — the media is content-protected and we
//!   decline to decrypt it.
//!
//! No resolver in this crate has a code path that bypasses either. That is
//! deliberate; see ADR-0005.

use std::sync::Arc;

use async_trait::async_trait;
use url::Url;

use crate::error::ResolveError;
use crate::model::Media;
use crate::net::client::HttpClient;

pub mod dash;
pub mod direct;
pub mod hls;
pub mod page;

pub use dash::DashResolver;
pub use direct::DirectResolver;
pub use hls::HlsResolver;
pub use page::PageResolver;

/// Maximum size of a document (manifest or HTML page) we will parse.
///
/// Manifests are kilobytes. A page claiming to be many megabytes is not a page
/// we should be reading into memory on a user's machine.
pub const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;

/// Turns a claimed URL into a [`Media`].
#[async_trait]
pub trait Resolver: Send + Sync {
    /// Cheap, no I/O. Whether this resolver handles `url`.
    ///
    /// Must not perform network or filesystem work: it runs on every URL the
    /// registry sees, and once per registry to build its ordering.
    fn matches(&self, url: &Url) -> bool;

    /// Human-readable name, used in error messages and `ifami doctor` output.
    fn name(&self) -> &'static str;

    /// Resolve `url`. Called only when [`Resolver::matches`] returned `true`.
    async fn resolve(&self, client: &dyn HttpClient, url: &Url) -> Result<Media, ResolveError>;
}

/// The ordered set of resolvers.
#[derive(Clone)]
pub struct ResolverSet {
    resolvers: Vec<Arc<dyn Resolver>>,
}

impl ResolverSet {
    /// An empty set. Resolving with it always yields
    /// [`ResolveError::UnsupportedScheme`].
    pub fn empty() -> Self {
        Self {
            resolvers: Vec::new(),
        }
    }

    /// The default set, in the order resolvers must be tried.
    ///
    /// Order matters: a URL ending in `.m3u8` must be handled by the HLS
    /// resolver before the direct resolver sees it, or a segmented stream would
    /// be fetched as one opaque file.
    pub fn with_defaults() -> Self {
        Self::empty()
            .with(Arc::new(HlsResolver::new()))
            .with(Arc::new(DashResolver::new()))
            .with(Arc::new(DirectResolver::new()))
            .with(Arc::new(PageResolver::new()))
    }

    /// Append a resolver.
    pub fn with(mut self, resolver: Arc<dyn Resolver>) -> Self {
        self.resolvers.push(resolver);
        self
    }

    /// The registered resolvers, in order.
    pub fn resolvers(&self) -> &[Arc<dyn Resolver>] {
        &self.resolvers
    }

    /// Names of the registered resolvers, in order.
    pub fn resolver_names(&self) -> Vec<&'static str> {
        self.resolvers.iter().map(|r| r.name()).collect()
    }

    /// Resolve `url` using the registered resolvers, in order.
    ///
    /// Every matching resolver is tried, not just the first, and the first
    /// success wins. Ordering expresses preference rather than exclusivity,
    /// which is what lets a URL like `clip.mp4` that actually serves an HTML
    /// page fall through from the direct resolver to page extraction. Without
    /// that, a perfectly ordinary mis-served link would fail.
    ///
    /// If every matching resolver fails, the error returned is chosen for
    /// usefulness rather than simply being the last one:
    ///
    /// 1. Any [`ResolveError::DrmProtected`] or [`ResolveError::AuthRequired`]
    ///    wins outright. These are deliberate scope decisions, and they are far
    ///    more actionable than a generic "no media found".
    /// 2. Otherwise, the first error that was not a plain
    ///    [`ResolveError::NoMedia`].
    /// 3. Otherwise, the first [`ResolveError::NoMedia`].
    pub async fn resolve(&self, client: &dyn HttpClient, url: &Url) -> Result<Media, ResolveError> {
        let matched: Vec<&Arc<dyn Resolver>> =
            self.resolvers.iter().filter(|r| r.matches(url)).collect();

        if matched.is_empty() {
            return Err(ResolveError::UnsupportedScheme {
                scheme: url.scheme().to_string(),
                url: url.to_string(),
            });
        }

        let mut first_no_media: Option<ResolveError> = None;
        let mut first_other: Option<ResolveError> = None;

        for resolver in matched {
            match resolver.resolve(client, url).await {
                Ok(media) if !media.formats.is_empty() => return Ok(media),
                Ok(_) => {
                    // A resolver that returns an empty media object has nothing
                    // useful; treat it as no media and keep going.
                    first_no_media.get_or_insert_with(|| ResolveError::NoMedia {
                        url: url.to_string(),
                    });
                }
                Err(err) => match err {
                    ResolveError::DrmProtected | ResolveError::AuthRequired => return Err(err),
                    ResolveError::NoMedia { .. } => {
                        first_no_media.get_or_insert(err);
                    }
                    other => {
                        if first_other.is_none() {
                            first_other = Some(other);
                        }
                    }
                },
            }
        }

        Err(first_other
            .or(first_no_media)
            .unwrap_or_else(|| ResolveError::NoMedia {
                url: url.to_string(),
            }))
    }
}

/// Detect well-known DRM markers in an arbitrary document.
///
/// Returns [`ResolveError::DrmProtected`] if any marker is present.
///
/// This is intentionally generous about what it matches: a false positive costs
/// the user one clear error message, whereas a false negative means we hand them
/// a stream we cannot download and they have no idea why.
pub fn detect_drm_markers(document: &str) -> bool {
    const MARKERS: &[&str] = &[
        "widevine",
        "com.widevine.alpha",
        "playready",
        "com.microsoft.playready",
        "com.apple.streamingkeydelivery",
        "fairplay",
        "skd://",
        "com.apple.fps",
        "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed",
        "urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95",
        "urn:uuid:94ce86fb-07ff-4f43-adb8-93d2fa968ca2",
        "clearkey",
        "org.w3.clearkey",
    ];

    let lowered = document.to_ascii_lowercase();
    MARKERS.iter().any(|m| lowered.contains(m))
}

/// Resolve `url` against `base`, used when a manifest references a relative URI.
pub fn join(base: &Url, reference: &str) -> Result<Url, ResolveError> {
    base.join(reference).map_err(|e| ResolveError::Malformed {
        url: reference.to_string(),
        reason: format!("could not resolve relative reference: {e}"),
    })
}

/// Turn a format into something the transfer engine can actually execute.
///
/// A master HLS playlist resolves to one format per rendition, each pointing at
/// a *variant* playlist that has not been fetched yet. Expanding it here, at
/// queue time rather than at transfer time, means the segment plan is recorded
/// in the queue file: a resumed download uses the plan it started with rather
/// than re-reading a playlist that may since have gained a segment.
///
/// Formats that are already executable are returned untouched.
pub async fn expand_segmented(
    client: &dyn HttpClient,
    format: &crate::model::Format,
) -> Result<crate::model::Format, ResolveError> {
    use crate::model::{Container, Format};

    if format.segments.is_some() {
        return Ok(format.clone());
    }

    if !matches!(format.container, Container::Hls) {
        return Ok(format.clone());
    }

    let url = Url::parse(&format.url).map_err(|e| ResolveError::Malformed {
        url: format.url.clone(),
        reason: e.to_string(),
    })?;

    let playlist = HlsResolver::new().fetch(client, &url).await?;

    if playlist.is_master {
        return Err(ResolveError::UnsupportedFeature {
            url: format.url.clone(),
            feature: "a master playlist must have one rendition chosen before it can be queued"
                .into(),
        });
    }

    let segments = playlist.segment_plan();

    if segments.is_empty() {
        return Err(ResolveError::NoMedia {
            url: format.url.clone(),
        });
    }

    // Now that the variant playlist has been read, the output container is known.
    // Leaving it as `Container::Hls` here would name the finished file `.bin`
    // even though the media playlist told us exactly what it is.
    Ok(Format {
        container: playlist.output_container(),
        segments: Some(segments),
        total_bytes: None,
        ..format.clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Never;

    #[async_trait]
    impl Resolver for Never {
        fn matches(&self, _: &Url) -> bool {
            false
        }
        fn name(&self) -> &'static str {
            "never"
        }
        async fn resolve(&self, _: &dyn HttpClient, _: &Url) -> Result<Media, ResolveError> {
            unreachable!("must not be called when matches() is false")
        }
    }

    /// Returns whatever outcome it was constructed with, and claims every URL.
    struct Scripted(Result<Media, ResolveError>, &'static str);

    #[async_trait]
    impl Resolver for Scripted {
        fn matches(&self, _: &Url) -> bool {
            true
        }
        fn name(&self) -> &'static str {
            self.1
        }
        async fn resolve(&self, _: &dyn HttpClient, _: &Url) -> Result<Media, ResolveError> {
            self.0.clone()
        }
    }

    fn media_with_one_format() -> Media {
        use crate::model::{Container, Format, MediaKind};
        let mut m = Media::new("id", "title", "https://example.invalid/x");
        m.formats.push(Format {
            id: "0".into(),
            url: "https://example.invalid/x.mp4".into(),
            container: Container::Mp4,
            kind: MediaKind::VideoWithAudio,
            quality: None,
            total_bytes: None,
            content_hash: None,
            mime: Some("video/mp4".into()),
            requires_mux: false,
            segments: None,
        });
        m
    }

    fn no_media() -> ResolveError {
        ResolveError::NoMedia {
            url: "https://example.invalid/x".into(),
        }
    }

    /// A transport that panics if used. Registry tests must not touch the
    /// network, and a client that screams if it is reached is how that stays
    /// true rather than merely being intended.
    struct NoNetwork;

    #[async_trait]
    impl HttpClient for NoNetwork {
        async fn execute(
            &self,
            req: crate::net::client::HttpRequest,
        ) -> std::result::Result<crate::net::client::RawResponse, crate::error::NetError> {
            panic!(
                "registry tests must not perform I/O, but requested {}",
                req.url
            )
        }
    }

    #[tokio::test]
    async fn unclaimed_urls_report_the_scheme() {
        let set = ResolverSet::empty();
        let url = Url::parse("https://example.invalid/x").unwrap();
        let err = set.resolve(&NoNetwork, &url).await.unwrap_err();
        assert!(matches!(err, ResolveError::UnsupportedScheme { .. }));
    }

    #[tokio::test]
    async fn the_default_registry_claims_ordinary_urls_in_order() {
        let names = ResolverSet::with_defaults().resolver_names();
        assert_eq!(
            names,
            vec!["hls", "dash", "direct", "page"],
            "a segmented stream must be claimed before the direct resolver, \
             or a manifest would be fetched as one opaque file"
        );

        let set = ResolverSet::with_defaults();
        let url = Url::parse("https://cdn.example.invalid/a/b.m3u8").unwrap();
        let claimants: Vec<&str> = set
            .resolvers()
            .iter()
            .filter(|r| r.matches(&url))
            .map(|r| r.name())
            .collect();
        // Every matching resolver is tried, not just the first, so a
        // mis-served `.m3u8` can still fall through to page extraction.
        assert_eq!(claimants.first(), Some(&"hls"));
    }

    #[tokio::test]
    async fn a_resolver_that_finds_nothing_does_not_stop_the_registry() {
        // The fallthrough that matters: a URL named `clip.mp4` that actually
        // serves an HTML page fails in the direct resolver and must still be
        // handed to the page resolver rather than reported as "no media".
        let set = ResolverSet::empty()
            .with(Arc::new(Scripted(Err(no_media()), "direct")))
            .with(Arc::new(Scripted(Ok(media_with_one_format()), "page")));
        let url = Url::parse("https://cdn.example.invalid/clip.mp4").unwrap();

        let media = set.resolve(&NoNetwork, &url).await.expect("fell through");
        assert_eq!(media.formats.len(), 1);
    }

    #[tokio::test]
    async fn a_scope_refusal_beats_a_generic_no_media_from_another_resolver() {
        // The most important ordering rule in the file: DRM and AuthRequired are
        // deliberate decisions, and reporting "no media found" instead would hide
        // the reason from the user.
        let set = ResolverSet::empty()
            .with(Arc::new(Scripted(Err(no_media()), "direct")))
            .with(Arc::new(Scripted(Err(ResolveError::DrmProtected), "page")));

        let url = Url::parse("https://cdn.example.invalid/movie.mp4").unwrap();
        let err = set.resolve(&NoNetwork, &url).await.unwrap_err();
        assert!(
            matches!(err, ResolveError::DrmProtected),
            "got {err:?} instead of the scope refusal"
        );
    }

    #[tokio::test]
    async fn a_specific_failure_outranks_a_plain_no_media() {
        let set = ResolverSet::empty()
            .with(Arc::new(Scripted(Err(no_media()), "first")))
            .with(Arc::new(Scripted(
                Err(ResolveError::ResponseTooLarge {
                    url: "https://example.invalid/x".into(),
                    limit: 10,
                }),
                "second",
            )));

        let url = Url::parse("https://cdn.example.invalid/a").unwrap();
        let err = set.resolve(&NoNetwork, &url).await.unwrap_err();
        assert!(
            matches!(err, ResolveError::ResponseTooLarge { .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn a_media_with_no_formats_counts_as_no_media() {
        let bare = Media::new("id", "title", "https://example.invalid/x");
        let set = ResolverSet::empty()
            .with(Arc::new(Scripted(Ok(bare), "only")))
            .with(Arc::new(Scripted(Ok(media_with_one_format()), "backup")));

        let url = Url::parse("https://cdn.example.invalid/a").unwrap();
        let media = set.resolve(&NoNetwork, &url).await.expect("fell through");
        assert_eq!(media.formats.len(), 1);
    }

    #[tokio::test]
    async fn a_resolver_that_does_not_claim_a_url_is_never_called() {
        let set = ResolverSet::empty().with(Arc::new(Never));
        let url = Url::parse("https://cdn.example.invalid/a").unwrap();
        // `Never::resolve` is `unreachable!()`, so reaching this point at all
        // proves the registry filtered on `matches` before calling.
        assert!(set.resolve(&NoNetwork, &url).await.is_err());
    }

    #[test]
    fn drm_marker_detection_covers_the_major_systems() {
        for marker in [
            "<video><track>widevine</track>",
            "keySystem com.microsoft.playready",
            "skd://abc",
            "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed",
        ] {
            assert!(detect_drm_markers(marker), "missed {marker:?}");
        }
    }

    #[test]
    fn drm_detection_is_case_insensitive_and_does_not_fire_on_normal_markup() {
        assert!(detect_drm_markers("WIDEVINE"));
        assert!(!detect_drm_markers(
            "<video src=\"movie.mp4\" controls></video>"
        ));
        assert!(!detect_drm_markers("application/dash+xml"));
    }

    #[test]
    fn relative_references_resolve_against_the_manifest() {
        let base = Url::parse("https://cdn.example.invalid/a/b/master.m3u8").unwrap();
        assert_eq!(
            join(&base, "seg1.ts").unwrap().as_str(),
            "https://cdn.example.invalid/a/b/seg1.ts"
        );
        assert_eq!(
            join(&base, "/root/x.ts").unwrap().as_str(),
            "https://cdn.example.invalid/root/x.ts"
        );
        assert_eq!(
            join(&base, "https://other.invalid/y.ts").unwrap().as_str(),
            "https://other.invalid/y.ts"
        );
    }
}
