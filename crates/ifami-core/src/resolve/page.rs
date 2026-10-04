//! Extract media references from an HTML page.
//!
//! For self-hosted and openly-licensed media, a page is usually the only place
//! the real asset URL appears. This resolver reads a page and reports the media
//! it advertises.
//!
//! It deliberately does **not** execute JavaScript, replay API calls, or
//! reproduce signed requests. A page whose media is only obtainable by running
//! script is reported as [`ResolveError::NoMedia`], which is the correct answer
//! under `docs/SCOPE.md` — see ADR-0005.
//!
//! No browser extension is involved or required (ADR-0004). We fetch the page
//! ourselves, which is permitted and works without one.

use async_trait::async_trait;
use scraper::{Html, Selector};
use url::Url;

use crate::error::ResolveError;
use crate::model::{Container, Format, Media, MediaKind};
use crate::net::client::HttpClient;
use crate::resolve::direct::{id_for, looks_like_media_path, MEDIA_EXTENSIONS};
use crate::resolve::hls::fetch_text;
use crate::resolve::{detect_drm_markers, join, Resolver};

/// Extensions treated as a media reference when found in a link or source tag.
fn is_media_ref(url: &Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    if url.path().to_ascii_lowercase().ends_with(".m3u8") {
        return true;
    }
    if url.path().to_ascii_lowercase().ends_with(".mpd") {
        return true;
    }
    looks_like_media_path(url)
}

/// Add a candidate reference, deduplicated.
///
/// `authored` marks references where the page itself asserted this is media —
/// an `og:video:url`, or an `<a download>`. Those are taken even without a media
/// extension, because `/stream?id=42` is a perfectly normal way to publish a
/// clip and rejecting it on a technicality would make the feature useless.
/// Everything else must look like media, or every sidebar link and thumbnail
/// becomes a "format".
fn push_ref(
    found: &mut Vec<PageMedia>,
    seen: &mut std::collections::BTreeSet<String>,
    base: &Url,
    raw: &str,
    mime: Option<String>,
    origin: &str,
    authored: bool,
) {
    let Ok(abs) = join(base, raw) else { return };
    if !matches!(abs.scheme(), "http" | "https") {
        return;
    }
    if !authored && !is_media_ref(&abs) {
        return;
    }

    let key = abs.to_string();
    if seen.insert(key.clone()) {
        found.push(PageMedia {
            url: key,
            mime,
            origin: origin.to_string(),
        });
    }
}

/// Resolves media advertised by an HTML page.
#[derive(Debug, Clone, Copy, Default)]
pub struct PageResolver;

impl PageResolver {
    /// Create the resolver.
    pub fn new() -> Self {
        Self
    }
}

/// A media reference found on a page, with a hint about where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageMedia {
    /// Absolute URL of the media.
    pub url: String,
    /// MIME type, when the page declared one.
    pub mime: Option<String>,
    /// Where the reference was found, for user-facing diagnostics.
    ///
    /// Owned rather than `&'static str` because one of the origins is the
    /// document's own `property` attribute, which lives exactly as long as the
    /// parsed page does.
    pub origin: String,
}

/// Extract every media reference advertised by `html`.
///
/// Ordering is significant: earlier origins are more likely to be the intended
/// media. `<video src>` beats `<source>` beats OpenGraph beats a JSON-LD
/// `contentUrl` beats an incidental `<a href>` to a file with a media
/// extension.
pub fn extract_media(html: &str, base: &Url) -> Result<Vec<PageMedia>, ResolveError> {
    if detect_drm_markers(html) {
        return Err(ResolveError::DrmProtected);
    }

    let doc = Html::parse_document(html);
    let mut found: Vec<PageMedia> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    // 1. <video src> and <audio src>
    for (tag, origin) in [("video", "video element"), ("audio", "audio element")] {
        let sel = Selector::parse(&format!("{tag}[src]")).expect("static selector is valid");
        for el in doc.select(&sel) {
            let Some(src) = el.value().attr("src") else {
                continue;
            };
            let mime = mime_from_type_attr(el.value().attr("type"));
            push_ref(&mut found, &mut seen, base, src, mime, origin, false);
        }
    }

    // 2. <source src type=...> inside media elements
    let source_sel = Selector::parse("video source[src], audio source[src]").expect("static");
    for el in doc.select(&source_sel) {
        let Some(src) = el.value().attr("src") else {
            continue;
        };
        let mime = mime_from_type_attr(el.value().attr("type"));
        push_ref(
            &mut found,
            &mut seen,
            base,
            src,
            mime,
            "source element",
            false,
        );
    }

    // 3. <link rel="alternate" type="application/x-mpegURL">
    let link_sel = Selector::parse(r#"link[rel="alternate"][href], link[rel="preload"][href]"#)
        .expect("static selector is valid");
    for el in doc.select(&link_sel) {
        let Some(href) = el.value().attr("href") else {
            continue;
        };
        let mime = mime_from_type_attr(el.value().attr("type"));
        let rel = el.value().attr("rel").unwrap_or_default();
        let origin = if rel.eq_ignore_ascii_case("preload") {
            "preload link"
        } else {
            "alternate link"
        };
        push_ref(&mut found, &mut seen, base, href, mime, origin, false);
    }

    // 4. OpenGraph video/audio
    let og_sel =
        Selector::parse(r#"meta[property="og:video:url"], meta[property="og:video:secure_url"], meta[property="og:audio"], meta[property="og:audio:secure_url"], meta[property="twitter:player:stream"]"#)
            .expect("static selector is valid");
    for el in doc.select(&og_sel) {
        let Some(content) = el.value().attr("content") else {
            continue;
        };
        let prop = el.value().attr("property").unwrap_or("opengraph");
        push_ref(&mut found, &mut seen, base, content, None, prop, true);
    }

    // 5. JSON-LD contentUrl / embedUrl
    let ld_sel = Selector::parse(r#"script[type="application/ld+json"]"#).expect("static");
    for el in doc.select(&ld_sel) {
        let text = el.text().collect::<String>();
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            // Malformed JSON-LD is extremely common and must not fail the page.
            continue;
        };
        let mut urls = Vec::new();
        collect_json_ld_urls(&value, &mut urls, 0);
        for u in urls {
            push_ref(&mut found, &mut seen, base, &u, None, "json-ld", false);
        }
    }

    // 6. Download links and incidental references.
    //
    // Deliberately last. Only `download` links and hrefs with a media extension
    // qualify; scanning every href produces false positives from navigation and
    // ad markup, which makes the feature useless.
    let a_sel = Selector::parse("a[href]").expect("static selector is valid");
    for el in doc.select(&a_sel) {
        let Some(href) = el.value().attr("href") else {
            continue;
        };
        let is_download = el
            .value()
            .attr("download")
            .map(|v| !v.is_empty() && v != "false")
            .unwrap_or(false);
        let lower = href.split('?').next().unwrap_or("").to_ascii_lowercase();
        let media_ext = MEDIA_EXTENSIONS.iter().any(|e| {
            lower.ends_with(&format!(".{e}")) || lower.ends_with(".m3u8") || lower.ends_with(".mpd")
        });
        if !is_download && !media_ext {
            continue;
        }
        push_ref(
            &mut found,
            &mut seen,
            base,
            href,
            None,
            if is_download {
                "download link"
            } else {
                "media link"
            },
            is_download,
        );
    }

    Ok(found)
}

/// Walk a JSON-LD value collecting `contentUrl` and `embedUrl` string values.
///
/// Depth-limited: a hostile or malformed document must not be able to cause
/// unbounded recursion.
fn collect_json_ld_urls(value: &serde_json::Value, out: &mut Vec<String>, depth: usize) {
    const MAX_DEPTH: usize = 12;
    if depth > MAX_DEPTH {
        return;
    }
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                match k.as_str() {
                    "contentUrl" | "embedUrl" | "@id" if v.is_string() => {
                        if let Some(s) = v.as_str() {
                            // `@id` is a node identifier, not a URL, in the common
                            // case; only take it if it looks like one.
                            if k == "@id" && !s.starts_with("http") {
                                continue;
                            }
                            out.push(s.to_string());
                        }
                    }
                    _ => collect_json_ld_urls(v, out, depth + 1),
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_json_ld_urls(item, out, depth + 1);
            }
        }
        _ => {}
    }
}

/// Normalise a `type` attribute into a bare MIME type.
fn mime_from_type_attr(t: Option<&str>) -> Option<String> {
    let raw = t?.split(';').next().unwrap_or("").trim();
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_ascii_lowercase())
    }
}

/// Page title, for a friendly default label.
///
/// Both sources go through the same collapse: real pages carry tabs, newlines,
/// and doubled spaces inside `<title>`, and the label ends up in a filename, a
/// row header, and a window title. `Fallback   Title` looks like a bug.
fn collapse(input: &str) -> Option<String> {
    let joined = input.split_whitespace().collect::<Vec<_>>().join(" ");
    (!joined.is_empty()).then_some(joined)
}

/// Page title, for a friendly default label.
pub fn extract_title(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    let sel = Selector::parse("title").ok()?;
    let title = doc
        .select(&sel)
        .next()
        .map(|t| t.text().collect::<String>())
        .as_deref()
        .and_then(collapse);
    if title.is_some() {
        return title;
    }
    let og = Selector::parse(r#"meta[property="og:title"]"#).ok()?;
    doc.select(&og)
        .next()
        .and_then(|m| m.value().attr("content"))
        .and_then(collapse)
}

#[async_trait]
impl Resolver for PageResolver {
    fn matches(&self, url: &Url) -> bool {
        // The page resolver is the catch-all: it claims any http(s) URL. The
        // registry tries matchers in order and takes the first success, so
        // claiming broadly is safe and is what lets a `clip.mp4` URL that
        // actually serves HTML fall through to page extraction.
        matches!(url.scheme(), "http" | "https")
    }

    fn name(&self) -> &'static str {
        "page"
    }

    async fn resolve(&self, client: &dyn HttpClient, url: &Url) -> Result<Media, ResolveError> {
        let html = fetch_text(client, url).await?;

        let found = extract_media(&html, url)?;
        if found.is_empty() {
            return Err(ResolveError::NoMedia {
                url: url.to_string(),
            });
        }

        let mut media = Media::new(
            id_for(url),
            extract_title(&html).unwrap_or_else(|| "media".to_string()),
            url.as_str(),
        );

        for (i, m) in found.iter().enumerate() {
            let mime = m.mime.clone().unwrap_or_default();
            let container = {
                let c = Container::from_mime(&mime);
                if matches!(c, Container::Other(_)) {
                    Url::parse(&m.url)
                        .ok()
                        .and_then(|u| crate::resolve::direct::container_from_path(&u))
                        .unwrap_or(c)
                } else {
                    c
                }
            };

            media.formats.push(Format {
                id: format!("{i}"),
                url: m.url.clone(),
                container,
                kind: if mime.starts_with("audio/") {
                    MediaKind::AudioOnly
                } else {
                    MediaKind::VideoWithAudio
                },
                quality: None,
                total_bytes: None,
                content_hash: None,
                mime: if mime.is_empty() { None } else { Some(mime) },
                requires_mux: false,
                segments: None,
            });
        }

        Ok(media)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://example.invalid/watch/index.html").unwrap()
    }

    #[test]
    fn a_video_element_wins_over_incidental_links() {
        let html = r#"
            <html><head><title>My Clip</title></head><body>
              <a href="/assets/background.mp3">bg music</a>
              <video src="/media/clip.mp4" controls></video>
            </body></html>"#;
        let found = extract_media(html, &base()).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].url, "https://example.invalid/media/clip.mp4");
        assert_eq!(found[0].origin, "video element");
        // The incidental audio link is still reported, but after the real media.
        assert_eq!(
            found[1].url,
            "https://example.invalid/assets/background.mp3"
        );
    }

    #[test]
    fn source_elements_are_read() {
        let html = r#"<video>
            <source src="/a.webm" type="video/webm; codecs=vp9">
            <source src="/a.mp4" type="video/mp4">
          </video>"#;
        let found = extract_media(html, &base()).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].mime.as_deref(), Some("video/webm"));
        assert_eq!(found[1].url, "https://example.invalid/a.mp4");
    }

    #[test]
    fn relative_references_resolve_against_the_page() {
        let html = r#"<video src="clip.mp4"></video>"#;
        let found = extract_media(html, &base()).unwrap();
        assert_eq!(found[0].url, "https://example.invalid/watch/clip.mp4");
    }

    #[test]
    fn opengraph_video_is_found() {
        let html =
            r#"<meta property="og:video:secure_url" content="https://cdn.example.invalid/v.mp4">"#;
        let found = extract_media(html, &base()).unwrap();
        assert_eq!(found[0].url, "https://cdn.example.invalid/v.mp4");
    }

    #[test]
    fn json_ld_content_url_is_found_and_handles_arrays() {
        let html = r#"<script type="application/ld+json">
          {"@graph":[{"@type":"VideoObject","contentUrl":"https://cdn.example.invalid/v.mp4"},
                     {"@type":"ImageObject","contentUrl":"https://cdn.example.invalid/t.jpg"}]}
        </script>"#;
        let found = extract_media(html, &base()).unwrap();
        assert_eq!(found.len(), 1, "the image is not media");
        assert_eq!(found[0].url, "https://cdn.example.invalid/v.mp4");
        assert_eq!(found[0].origin, "json-ld");
    }

    #[test]
    fn malformed_json_ld_does_not_fail_the_page() {
        let html = r#"<script type="application/ld+json">{ not json </script>
            <video src="/a.mp4"></video>"#;
        let found = extract_media(html, &base()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].url, "https://example.invalid/a.mp4");
    }

    #[test]
    fn json_ld_walk_is_depth_limited() {
        // A hostile document must not be able to blow the stack.
        let mut v = serde_json::json!("leaf");
        for _ in 0..500 {
            v = serde_json::json!({ "contentUrl": "https://x.invalid/a.mp4", "child": v });
        }
        let mut out = Vec::new();
        collect_json_ld_urls(&v, &mut out, 0);
        assert!(out.len() <= 500, "walk did not stop");
    }

    #[test]
    fn duplicates_are_collapsed() {
        let html = r#"<video src="/a.mp4"></video>
            <meta property="og:video" content="/a.mp4">
            <a href="/a.mp4">download</a>"#;
        let found = extract_media(html, &base()).unwrap();
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn drm_signals_are_refused_before_anything_else() {
        let html = r#"<html><body>
            <video src="/a.mp4" data-key-system="com.widevine.alpha"></video>
        </body></html>"#;
        assert!(matches!(
            extract_media(html, &base()).unwrap_err(),
            ResolveError::DrmProtected
        ));
    }

    #[test]
    fn a_page_with_nothing_referential_yields_no_media() {
        let found = extract_media("<html><body><p>hello</p></body></html>", &base()).unwrap();
        assert!(found.is_empty());
    }

    #[test]
    fn navigation_links_are_not_treated_as_media() {
        // Only `download` links and media-extension hrefs qualify, or every
        // sidebar link becomes a "format".
        let html = r#"<a href="/about">About</a><a href="/contact.html">Contact</a>"#;
        assert!(extract_media(html, &base()).unwrap().is_empty());
    }

    #[test]
    fn download_attributes_are_honoured_even_without_a_media_extension() {
        let html = r#"<a href="/stream?id=42" download="clip.mp4">Get it</a>"#;
        let found = extract_media(html, &base()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].origin, "download link");
    }

    #[test]
    fn alternate_links_for_hls_are_found() {
        let html = r#"<link rel="alternate" type="application/x-mpegURL" href="/master.m3u8">"#;
        let found = extract_media(html, &base()).unwrap();
        assert_eq!(found[0].url, "https://example.invalid/master.m3u8");
        assert_eq!(found[0].mime.as_deref(), Some("application/x-mpegurl"));
    }

    #[test]
    fn title_falls_back_to_opengraph() {
        let html = r#"<meta property="og:title" content="Fallback   Title"><title></title>"#;
        assert_eq!(extract_title(html).as_deref(), Some("Fallback Title"));
    }

    #[test]
    fn titles_have_whitespace_normalised() {
        assert_eq!(
            extract_title("<title>  Spaced\n  Out  </title>").as_deref(),
            Some("Spaced Out")
        );
    }

    #[test]
    fn page_resolver_is_the_catch_all_for_http_urls() {
        // Claiming broadly is safe because the registry tries matchers in order
        // and takes the first success.
        let r = PageResolver::new();
        assert!(r.matches(&Url::parse("https://x.invalid/watch").unwrap()));
        assert!(r.matches(&Url::parse("https://x.invalid/a.m3u8").unwrap()));
        assert!(r.matches(&Url::parse("https://x.invalid/a.mp4").unwrap()));
        assert!(!r.matches(&Url::parse("file:///c/a.mp4").unwrap()));
    }
}
