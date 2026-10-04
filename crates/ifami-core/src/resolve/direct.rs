//! Direct single-file media: a URL that points straight at the bytes.
//!
//! This is the case that matters most for self-hosted and openly-licensed
//! media, and it is the one where the download manager's resume machinery does
//! the real work.
//!
//! One request establishes everything: whether the source can be resumed, its
//! total length, and its content type. There is no second round trip.

use async_trait::async_trait;
use url::Url;

use crate::error::{NetError, ResolveError};
use crate::model::{Container, Format, Media, MediaKind};
use crate::net::capability::probe;
use crate::net::client::HttpClient;
use crate::resolve::{detect_drm_markers, Resolver};

/// Path extensions treated as direct media.
pub const MEDIA_EXTENSIONS: &[&str] = &[
    // video
    "mp4", "m4v", "webm", "mkv", "mov", "avi", "mpg", "mpeg", "m2ts", "ts", "flv", "ogv",
    // audio
    "mp3", "m4a", "aac", "flac", "wav", "ogg", "oga", "opus", "weba", "aiff", "wma",
    // subtitles, which people legitimately want offline
    "vtt", "srt", "ass", "ssa",
];

/// MIME types that unambiguously identify direct media.
const MEDIA_MIMES: &[&str] = &[
    "video/",
    "audio/",
    "application/octet-stream",
    "application/mp4",
    "application/x-mpegurl",
];

/// Resolves direct media URLs.
#[derive(Debug, Clone, Copy, Default)]
pub struct DirectResolver;

impl DirectResolver {
    /// Create the resolver.
    pub fn new() -> Self {
        Self
    }
}

/// Whether `url`'s path ends in a recognised media extension.
///
/// Path extension only. The query string is ignored: `?file=x.mp4` is a page
/// that returns HTML, and treating it as media would send a download request
/// for a file that turns out to be markup.
pub fn looks_like_media_path(url: &Url) -> bool {
    match url.path_segments().and_then(|mut s| s.next_back()) {
        Some(last) => {
            let Some((stem, ext)) = last.rsplit_once('.') else {
                return false;
            };
            // Reject dotfiles like `.mp4` and empty stems.
            !stem.is_empty() && MEDIA_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
        }
        None => false,
    }
}

/// Container implied by the URL path, if any.
pub fn container_from_path(url: &Url) -> Option<Container> {
    let last = url.path_segments()?.next_back()?;
    let (_, ext) = last.rsplit_once('.')?;
    match ext.to_ascii_lowercase().as_str() {
        "mp4" | "m4v" => Some(Container::Mp4),
        "webm" => Some(Container::Webm),
        "mkv" => Some(Container::Matroska),
        "mov" | "qt" => Some(Container::Other("video/quicktime".into())),
        "mp3" => Some(Container::Mp3),
        "m4a" => Some(Container::Adts),
        "aac" => Some(Container::Adts),
        "flac" => Some(Container::Flac),
        "wav" => Some(Container::Wav),
        "ogg" | "oga" => Some(Container::Ogg),
        "opus" | "weba" => Some(Container::Opus),
        _ => None,
    }
}

/// Whether a MIME type identifies direct media.
pub fn mime_is_media(mime: &str) -> bool {
    let base = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    MEDIA_MIMES.iter().any(|m| base.starts_with(m))
}

#[async_trait]
impl Resolver for DirectResolver {
    fn matches(&self, url: &Url) -> bool {
        if !matches!(url.scheme(), "http" | "https") {
            return false;
        }
        if url.path().ends_with(".m3u8") {
            // Handled by the HLS resolver, which must run first.
            return false;
        }
        if url.path().ends_with(".mpd") {
            return false;
        }
        looks_like_media_path(url)
    }

    fn name(&self) -> &'static str {
        "direct"
    }

    async fn resolve(&self, client: &dyn HttpClient, url: &Url) -> Result<Media, ResolveError> {
        let caps = probe(client, url.as_str()).await.map_err(map_net)?;

        let mime = caps.content_type.clone().unwrap_or_default();
        if !mime.is_empty() && !mime_is_media(&mime) {
            // The path said `.mp4` but the server said `text/html`. That is the
            // classic signature of a link that leads to a page, not a file.
            return Err(ResolveError::NoMedia {
                url: url.to_string(),
            });
        }

        let container = Container::from_mime(&mime);
        let container = if matches!(container, Container::Other(_)) {
            container_from_path(url).unwrap_or(container)
        } else {
            container
        };

        let kind = if container.extension() == "bin" && mime.is_empty() {
            MediaKind::Unknown
        } else if mime.starts_with("audio/") {
            MediaKind::AudioOnly
        } else if mime.starts_with("video/") {
            MediaKind::VideoWithAudio
        } else {
            // No content type: fall back to the extension.
            match container.extension() {
                "mp3" | "aac" | "flac" | "wav" | "ogg" | "opus" => MediaKind::AudioOnly,
                "vtt" | "srt" | "ass" | "ssa" => MediaKind::Unknown,
                _ => MediaKind::VideoWithAudio,
            }
        };

        let title = url
            .path_segments()
            .and_then(|mut s| s.next_back())
            .and_then(|s| s.rsplit_once('.').map(|(stem, _)| stem))
            .filter(|s| !s.is_empty())
            .unwrap_or("media")
            .to_string();

        let mut media = Media::new(id_for(url), title, url.as_str());
        media.formats.push(Format {
            id: "0".to_string(),
            url: url.to_string(),
            container,
            kind,
            quality: None,
            total_bytes: caps.total_bytes,
            content_hash: None,
            mime: if mime.is_empty() { None } else { Some(mime) },
            requires_mux: false,
            segments: None,
        });
        Ok(media)
    }
}

/// Stable id derived from the URL.
///
/// Deliberately not a hash of the URL *string* alone: the same object served
/// from two hosts should be recognisably the same object. Hashing the URL is
/// what we have without contacting the source, and the queue reconciles by
/// content hash when one is advertised, so this only needs to be stable.
pub fn id_for(url: &Url) -> String {
    crate::digest(8, url.as_str())
}

/// Detect DRM markers in a media response before offering the format.
///
/// Used by resolvers that read a document body. A `DrmProtected` result here is
/// a refusal, not a failure.
pub fn refuse_if_drm(body: &str) -> Result<(), ResolveError> {
    if detect_drm_markers(body) {
        return Err(ResolveError::DrmProtected);
    }
    Ok(())
}

fn map_net(e: NetError) -> ResolveError {
    match e {
        // 401 and 403 are a gate, not a broken link. Reporting them as a generic
        // failure would make a client retry a source we have deliberately
        // declined to access.
        NetError::Status {
            status: 401 | 403, ..
        } => ResolveError::AuthRequired,
        NetError::Status { status, url } => ResolveError::NoMedia {
            url: format!("{url} (status {status})"),
        },
        other => ResolveError::Malformed {
            url: String::new(),
            reason: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::client::{Headers, RawResponse};
    use std::sync::Mutex;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn path_extension_detection() {
        assert!(looks_like_media_path(&url("https://x.invalid/a/b.mp4")));
        assert!(looks_like_media_path(&url("https://x.invalid/a/b.MP4")));
        assert!(looks_like_media_path(&url("https://x.invalid/a/track.mp3")));
        assert!(looks_like_media_path(&url(
            "https://x.invalid/captions.vtt"
        )));
    }

    #[test]
    fn query_string_does_not_make_a_page_look_like_media() {
        // `?file=x.mp4` returns HTML in practice. Fetching it as media wastes a
        // download and produces a file that will not open.
        assert!(!looks_like_media_path(&url(
            "https://x.invalid/watch?file=x.mp4"
        )));
        assert!(!looks_like_media_path(&url(
            "https://x.invalid/a/b.html?x=.mp4"
        )));
    }

    #[test]
    fn dotfiles_and_empty_stems_are_not_media() {
        assert!(!looks_like_media_path(&url("https://x.invalid/.mp4")));
        assert!(!looks_like_media_path(&url("https://x.invalid/.hidden")));
        assert!(!looks_like_media_path(&url("https://x.invalid/a/.mp4")));
    }

    #[test]
    fn trailing_slashes_and_bare_hosts_are_not_media() {
        assert!(!looks_like_media_path(&url("https://x.invalid/")));
        assert!(!looks_like_media_path(&url("https://x.invalid")));
        assert!(!looks_like_media_path(&url("https://x.invalid/a/b")));
    }

    #[test]
    fn manifest_urls_are_left_to_their_resolvers() {
        let r = DirectResolver::new();
        assert!(!r.matches(&url("https://x.invalid/master.m3u8")));
        assert!(!r.matches(&url("https://x.invalid/manifest.mpd")));
        assert!(r.matches(&url("https://x.invalid/a.mp4")));
    }

    #[test]
    fn non_http_schemes_are_never_matched() {
        let r = DirectResolver::new();
        assert!(!r.matches(&url("file:///c/a.mp4")));
        assert!(!r.matches(&url("ftp://x.invalid/a.mp4")));
    }

    #[test]
    fn mime_media_detection_ignores_parameters() {
        assert!(mime_is_media("video/mp4"));
        assert!(mime_is_media("video/mp4; codecs=\"avc1.42E01E\""));
        assert!(mime_is_media("AUDIO/MPEG"));
        assert!(mime_is_media("application/octet-stream"));
        assert!(!mime_is_media("text/html"));
        assert!(!mime_is_media("application/xhtml+xml"));
        assert!(!mime_is_media(""));
    }

    #[test]
    fn container_falls_back_to_the_path_when_the_mime_is_unknown() {
        assert_eq!(
            container_from_path(&url("https://x.invalid/a.webm")),
            Some(Container::Webm)
        );
        assert_eq!(container_from_path(&url("https://x.invalid/a.bin")), None);
    }

    #[test]
    fn ids_are_stable_and_url_dependent() {
        let a = id_for(&url("https://x.invalid/a.mp4"));
        assert_eq!(a, id_for(&url("https://x.invalid/a.mp4")));
        assert_ne!(a, id_for(&url("https://x.invalid/b.mp4")));
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn drm_refusal_is_a_refusal_not_a_parse_error() {
        assert!(refuse_if_drm("<html>widevine</html>").is_err());
        assert!(refuse_if_drm("<html><video src='a.mp4'></video></html>").is_ok());
    }

    struct Scripted {
        responses: Mutex<Vec<RawResponse>>,
    }

    impl Scripted {
        fn new(responses: Vec<RawResponse>) -> Self {
            Self {
                responses: Mutex::new(responses),
            }
        }
    }

    #[async_trait]
    impl HttpClient for Scripted {
        async fn execute(
            &self,
            _: crate::net::client::HttpRequest,
        ) -> Result<RawResponse, NetError> {
            let mut r = self.responses.lock().unwrap();
            if r.is_empty() {
                panic!("ran out of scripted responses");
            }
            Ok(r.remove(0))
        }
    }

    fn resp(status: u16, pairs: &[(&str, &str)]) -> RawResponse {
        RawResponse::empty(
            status,
            "https://x.invalid/a.mp4",
            Headers::new(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string()))),
        )
    }

    #[tokio::test]
    async fn a_video_mp4_resolves_to_one_format() {
        let c = Scripted::new(vec![resp(
            206,
            &[
                ("Content-Type", "video/mp4"),
                ("Content-Range", "bytes 0-0/1048576"),
            ],
        )]);
        let media = DirectResolver::new()
            .resolve(&c, &url("https://x.invalid/a.mp4"))
            .await
            .unwrap();
        assert_eq!(media.formats.len(), 1);
        assert_eq!(media.formats[0].container, Container::Mp4);
        assert_eq!(media.formats[0].kind, MediaKind::VideoWithAudio);
        assert_eq!(media.formats[0].total_bytes, Some(1_048_576));
        assert_eq!(media.title, "a");
    }

    #[tokio::test]
    async fn a_403_resolves_as_auth_required_not_as_a_generic_failure() {
        // This distinction is what stops a client retrying a gate forever. See
        // `Error::is_retryable`.
        let c = Scripted::new(vec![resp(403, &[])]);
        let err = DirectResolver::new()
            .resolve(&c, &url("https://x.invalid/a.mp4"))
            .await
            .unwrap_err();
        assert!(matches!(err, ResolveError::AuthRequired));
    }

    #[tokio::test]
    async fn a_media_extension_serving_html_resolves_as_no_media() {
        // The `.mp4` link that is actually a landing page.
        let c = Scripted::new(vec![resp(
            200,
            &[("Content-Type", "text/html; charset=utf-8")],
        )]);
        let err = DirectResolver::new()
            .resolve(&c, &url("https://x.invalid/a.mp4"))
            .await
            .unwrap_err();
        assert!(matches!(err, ResolveError::NoMedia { .. }));
    }

    #[tokio::test]
    async fn audio_mime_produces_an_audio_only_format() {
        // The URL has no useful extension; the content type is the only signal.
        // `resolve` does not consult `matches`, so this exercises the type
        // inference directly.
        let c = Scripted::new(vec![resp(200, &[("Content-Type", "audio/mpeg")])]);
        let media = DirectResolver::new()
            .resolve(&c, &url("https://x.invalid/song.bin"))
            .await
            .expect("resolves from the content type alone");
        assert_eq!(media.formats[0].kind, MediaKind::AudioOnly);
        assert_eq!(media.formats[0].container, Container::Mp3);
    }
}
