//! DASH (ISO/IEC 23009-1) manifest parsing.
//!
//! # Scope of support
//!
//! Implemented:
//!
//! * `MPD` → `Period` → `AdaptationSet` → `Representation`, with `BaseURL`
//!   resolved at every level of nesting.
//! * Representations addressed by an explicit `SegmentList` of `SegmentURL`s,
//!   including `mediaRange` byte ranges.
//! * Single-file representations, where `BaseURL` points at a whole file.
//!
//! Deliberately unsupported, reported as
//! [`ResolveError::UnsupportedFeature`] rather than silently mishandled:
//!
//! * `SegmentTemplate/SegmentTimeline` (`S`, `t`, `d`, `r`). This is the most
//!   common production configuration and it needs a real timeline expansion
//!   pass. Guessing at it yields subtly wrong byte ranges, and a wrong byte
//!   range produces a corrupt file that looks plausible.
//!
//! # Encryption
//!
//! Any `ContentProtection` element yields [`ResolveError::DrmProtected`].

use async_trait::async_trait;
use quick_xml::events::Event;
use quick_xml::Reader;
use url::Url;

use crate::error::ResolveError;
use crate::model::{Container, Format, Media, MediaKind, Quality, Segment};
use crate::net::client::HttpClient;
use crate::resolve::direct::id_for;
use crate::resolve::hls::fetch_text;
use crate::resolve::{join, Resolver};

/// Resolves DASH manifests.
#[derive(Debug, Clone, Copy, Default)]
pub struct DashResolver;

impl DashResolver {
    /// Create the resolver.
    pub fn new() -> Self {
        Self
    }
}

/// A parsed DASH manifest.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DashManifest {
    /// Declared `profiles`, retained for diagnostics.
    pub profiles: Option<String>,

    /// Representations found, flattened across periods and adaptation sets.
    pub representations: Vec<Representation>,
}

/// One DASH representation with its resolved segment plan.
#[derive(Debug, Clone, PartialEq)]
pub struct Representation {
    /// `id` from the manifest, or a positional fallback when absent.
    pub id: String,
    /// Fully resolved base URL for this representation.
    pub base_url: String,
    /// `mimeType`, verbatim.
    pub mime: Option<String>,
    /// Declared bandwidth in bits per second.
    pub bandwidth: Option<u64>,
    /// Pixel width.
    pub width: Option<u16>,
    /// Pixel height.
    pub height: Option<u16>,
    /// Codec string, verbatim.
    pub codecs: Option<String>,
    /// `contentType`/`mimeType` indicates audio.
    pub is_audio: bool,
    /// `contentType`/`mimeType` indicates video.
    pub is_video: bool,
    /// Segment plan, present only when explicitly segmented.
    pub segments: Option<Vec<Segment>>,
}

/// Attribute values pulled off a `<Representation>` start tag.
#[derive(Debug, Clone)]
struct RepAttrs {
    id: Option<String>,
    mime: Option<String>,
    bandwidth: Option<u64>,
    width: Option<u16>,
    height: Option<u16>,
    codecs: Option<String>,
}

impl RepAttrs {
    fn from_start(e: &quick_xml::events::BytesStart<'_>) -> Self {
        Self {
            id: attr(e, b"id"),
            mime: attr(e, b"mimeType"),
            bandwidth: attr(e, b"bandwidth").and_then(|v| v.parse().ok()),
            width: attr(e, b"width").and_then(|v| v.parse().ok()),
            height: attr(e, b"height").and_then(|v| v.parse().ok()),
            codecs: attr(e, b"codecs"),
        }
    }
}

/// Attribute values pulled off an `<AdaptationSet>` start tag.
struct SetAttrs {
    mime: Option<String>,
    content_type: Option<String>,
}

impl SetAttrs {
    /// Whether this adaptation set carries audio.
    fn is_audio(&self) -> bool {
        self.content_type
            .as_deref()
            .map(|c| c.eq_ignore_ascii_case("audio"))
            .unwrap_or_else(|| {
                self.mime
                    .as_deref()
                    .is_some_and(|m| m.starts_with("audio/"))
            })
    }

    /// Whether this adaptation set carries video.
    fn is_video(&self) -> bool {
        self.content_type
            .as_deref()
            .map(|c| c.eq_ignore_ascii_case("video"))
            .unwrap_or_else(|| {
                self.mime
                    .as_deref()
                    .is_some_and(|m| m.starts_with("video/"))
            })
    }
}

/// Which element a `BaseURL` applies to.
///
/// DASH nests `BaseURL` at four levels and each one is scoped to its own parent
/// plus everything beneath it. Tracking the scope explicitly — rather than
/// inferring it from which URL variables happen to differ — is what stops a
/// Period-level `BaseURL` from being applied to a Representation and silently
/// fetching the wrong asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Mpd,
    Period,
    Set,
    Rep,
}

/// Parse an MPD document, resolving every `BaseURL` against `base`.
pub fn parse_manifest(text: &str, base: &Url) -> Result<DashManifest, ResolveError> {
    let malformed = |reason: String| ResolveError::Malformed {
        url: base.to_string(),
        reason,
    };

    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);

    let mut manifest = DashManifest::default();

    // `BaseURL` is additive and scoped: a Period's resolves against the MPD's,
    // an AdaptationSet's against its Period's, and a Representation's against
    // its AdaptationSet's.
    let mut mpd_base = base.clone();
    let mut period_base = base.clone();
    let mut set_base = base.clone();
    let mut rep_base = base.clone();
    let mut scope = Scope::Mpd;

    let mut set_attrs = SetAttrs {
        mime: None,
        content_type: None,
    };

    let mut rep_attrs = RepAttrs {
        id: None,
        mime: None,
        bandwidth: None,
        width: None,
        height: None,
        codecs: None,
    };
    let mut segments: Vec<Segment> = Vec::new();

    // `<BaseURL>text</BaseURL>` puts the URL in the element's *body*, so it has
    // to be read from the text events that follow the start tag. `SegmentURL`,
    // by contrast, carries its URL in a `media` attribute and is usually
    // self-closing. Handling both explicitly is the whole reason this parser
    // does not try to be clever.
    let mut base_url_scope: Option<Scope> = None;
    let mut base_url_text = String::new();
    let mut segment_pending: Option<(String, Option<String>)> = None;

    let mut buf = Vec::new();
    let mut saw_mpd = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(start)) => match local_name(start.name().as_ref()) {
                b"MPD" => {
                    saw_mpd = true;
                    manifest.profiles = attr(&start, b"profiles");
                    scope = Scope::Mpd;
                }
                b"BaseURL" => {
                    base_url_scope = Some(scope);
                    base_url_text.clear();
                }
                b"Period" => {
                    period_base = mpd_base.clone();
                    scope = Scope::Period;
                }
                b"AdaptationSet" => {
                    set_attrs = SetAttrs {
                        mime: attr(&start, b"mimeType"),
                        content_type: attr(&start, b"contentType"),
                    };
                    set_base = period_base.clone();
                    scope = Scope::Set;
                }
                b"Representation" => {
                    rep_attrs = RepAttrs::from_start(&start);
                    rep_base = set_base.clone();
                    segments.clear();
                    scope = Scope::Rep;
                }
                b"ContentProtection" => return Err(ResolveError::DrmProtected),
                b"SegmentTemplate" | b"SegmentTimeline" => {
                    // Refused rather than silently mishandled. A Representation
                    // using a template has no segment list we can execute, and
                    // falling through to the single-file path would fetch
                    // whatever `BaseURL` happens to point at — usually an
                    // initialisation segment — and hand the user a file that
                    // plays for one frame and then stops.
                    let feature = if local_name(start.name().as_ref()) == b"SegmentTemplate" {
                        "SegmentTemplate"
                    } else {
                        "SegmentTimeline"
                    };
                    return Err(ResolveError::UnsupportedFeature {
                        url: base.to_string(),
                        feature: format!(
                            "{feature}; ifami resolves DASH representations addressed by an \
                             explicit SegmentList only"
                        ),
                    });
                }
                b"SegmentURL" => {
                    segment_pending = Some((
                        attr(&start, b"media").unwrap_or_default(),
                        attr(&start, b"mediaRange"),
                    ));
                }
                _ => {}
            },

            Ok(Event::Empty(empty)) => match local_name(empty.name().as_ref()) {
                b"BaseURL" => {
                    // `<BaseURL href="..."/>` is a legal spelling.
                    if let Some(href) = attr(&empty, b"href") {
                        apply_base_url(
                            scope,
                            &href,
                            &mut mpd_base,
                            &mut period_base,
                            &mut set_base,
                            &mut rep_base,
                            &malformed,
                        )?;
                    }
                }
                b"ContentProtection" => return Err(ResolveError::DrmProtected),
                b"SegmentTemplate" | b"SegmentTimeline" => {
                    let feature = if local_name(empty.name().as_ref()) == b"SegmentTemplate" {
                        "SegmentTemplate"
                    } else {
                        "SegmentTimeline"
                    };
                    return Err(ResolveError::UnsupportedFeature {
                        url: base.to_string(),
                        feature: format!(
                            "{feature}; ifami resolves DASH representations addressed by an \
                             explicit SegmentList only"
                        ),
                    });
                }
                b"SegmentURL" => {
                    if let Some(uri) = attr(&empty, b"media") {
                        let byte_range = match attr(&empty, b"mediaRange") {
                            Some(raw) => match crate::net::range::parse_media_range(&raw) {
                                Ok(range) => Some(range),
                                Err(e) => {
                                    return Err(malformed(format!("bad mediaRange {raw:?}: {e}")))
                                }
                            },
                            None => None,
                        };
                        let uri = join(&rep_base, &uri)?;
                        segments.push(Segment {
                            index: segments.len() as u32,
                            uri: uri.to_string(),
                            byte_range,
                            duration_secs: None,
                        });
                    }
                }
                b"Representation" => {
                    // Self-closing: no children, therefore no segment list.
                    let attrs = RepAttrs::from_start(&empty);
                    manifest.representations.push(build(
                        attrs,
                        set_base.clone(),
                        &set_attrs,
                        None,
                        manifest.representations.len(),
                    ));
                }
                _ => {}
            },

            Ok(Event::Text(text)) => {
                if base_url_scope.is_some() {
                    base_url_text.push_str(&unescape(&String::from_utf8_lossy(text.as_ref())));
                }
            }

            Ok(Event::CData(data)) => {
                if base_url_scope.is_some() {
                    base_url_text.push_str(&String::from_utf8_lossy(data.as_ref()));
                }
            }

            Ok(Event::End(end)) => match local_name(end.name().as_ref()) {
                b"BaseURL" => {
                    if let Some(s) = base_url_scope.take() {
                        let raw = base_url_text.trim().to_string();
                        base_url_text.clear();
                        if !raw.is_empty() {
                            apply_base_url(
                                s,
                                &raw,
                                &mut mpd_base,
                                &mut period_base,
                                &mut set_base,
                                &mut rep_base,
                                &malformed,
                            )?;
                        }
                    }
                }
                b"SegmentURL" => {
                    if let Some((uri, media_range)) = segment_pending.take() {
                        if !uri.is_empty() {
                            // `mediaRange` is inclusive-end, unlike an HTTP
                            // request range.
                            let byte_range = match media_range {
                                Some(raw) => match crate::net::range::parse_media_range(&raw) {
                                    Ok(range) => Some(range),
                                    Err(e) => {
                                        return Err(malformed(format!(
                                            "bad mediaRange {raw:?}: {e}"
                                        )))
                                    }
                                },
                                None => None,
                            };
                            let uri = join(&rep_base, &uri)?;
                            segments.push(Segment {
                                index: segments.len() as u32,
                                uri: uri.to_string(),
                                byte_range,
                                duration_secs: None,
                            });
                        }
                    }
                }
                b"Representation" => {
                    let plan = (!segments.is_empty()).then(|| segments.clone());
                    manifest.representations.push(build(
                        rep_attrs.clone(),
                        rep_base.clone(),
                        &set_attrs,
                        plan,
                        manifest.representations.len(),
                    ));
                    segments.clear();
                    scope = Scope::Set;
                }
                b"AdaptationSet" => {
                    set_attrs = SetAttrs {
                        mime: None,
                        content_type: None,
                    };
                    scope = Scope::Period;
                }
                b"Period" => {
                    set_attrs = SetAttrs {
                        mime: None,
                        content_type: None,
                    };
                    period_base = mpd_base.clone();
                    scope = Scope::Mpd;
                }
                _ => {}
            },

            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(err) => return Err(malformed(err.to_string())),
        }
        buf.clear();
    }

    if !saw_mpd {
        return Err(malformed(
            "no <MPD> element; this is not a DASH manifest".into(),
        ));
    }
    if manifest.representations.is_empty() {
        return Err(ResolveError::NoMedia {
            url: base.to_string(),
        });
    }

    Ok(manifest)
}

/// Resolve a `BaseURL` against the base it is scoped to, and store it there.
///
/// The scope is passed in rather than read from parser state because the whole
/// point of tracking it is that a Period's `BaseURL` must not leak into a
/// Representation. Handing this function the wrong scope is the bug.
fn apply_base_url(
    scope: Scope,
    raw: &str,
    mpd_base: &mut Url,
    period_base: &mut Url,
    set_base: &mut Url,
    rep_base: &mut Url,
    malformed: &impl Fn(String) -> ResolveError,
) -> Result<(), ResolveError> {
    let target = match scope {
        Scope::Mpd => mpd_base,
        Scope::Period => period_base,
        Scope::Set => set_base,
        Scope::Rep => rep_base,
    };
    let resolved = join(target, raw).map_err(|e| match e {
        ResolveError::Malformed { reason, .. } => malformed(reason),
        other => other,
    })?;
    *target = resolved;
    Ok(())
}

fn build(
    attrs: RepAttrs,
    base_url: Url,
    set: &SetAttrs,
    segments: Option<Vec<Segment>>,
    ordinal: usize,
) -> Representation {
    let mime = attrs.mime.or_else(|| set.mime.clone());
    Representation {
        id: attrs
            .id
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("r{ordinal}")),
        base_url: base_url.to_string(),
        mime,
        bandwidth: attrs.bandwidth,
        width: attrs.width,
        height: attrs.height,
        codecs: attrs.codecs,
        is_audio: set.is_audio(),
        is_video: set.is_video(),
        segments,
    }
}

/// Strip namespace prefixes: `mpd:Period` is `Period`.
fn local_name(raw: &[u8]) -> &[u8] {
    match raw.iter().rposition(|b| *b == b':') {
        Some(i) => &raw[i + 1..],
        None => raw,
    }
}

/// Read an attribute as a trimmed `String`.
///
/// Decoded from raw bytes rather than via a configured XML decoder, so the
/// behaviour cannot drift with a `quick-xml` version bump. DASH attribute values
/// are URLs, integers and short tokens; handling the five predefined entities
/// covers everything that appears in practice.
fn attr(e: &quick_xml::events::BytesStart<'_>, name: &[u8]) -> Option<String> {
    for a in e.attributes().flatten() {
        if local_name(a.key.as_ref()) == name {
            let raw = String::from_utf8_lossy(a.value.as_ref());
            let decoded = unescape(&raw);
            let t = decoded.trim();
            if t.is_empty() {
                return None;
            }
            return Some(t.to_string());
        }
    }
    None
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[async_trait]
impl Resolver for DashResolver {
    fn matches(&self, url: &Url) -> bool {
        if !matches!(url.scheme(), "http" | "https") {
            return false;
        }
        url.path().to_ascii_lowercase().ends_with(".mpd")
    }

    fn name(&self) -> &'static str {
        "dash"
    }

    async fn resolve(&self, client: &dyn HttpClient, url: &Url) -> Result<Media, ResolveError> {
        let text = fetch_text(client, url).await?;
        let manifest = parse_manifest(&text, url)?;

        let mut media = Media::new(id_for(url), "dash", url.as_str());

        // A video-only rendition only needs muxing when the manifest *also*
        // offers an audio track that would have to be combined with it. Many
        // legitimate DASH presentations are video-only by design — a screen
        // recording, a silent film, a clip whose audio was never published —
        // and refusing those would be refusing the exact media we support best.
        // Deciding this per-representation would mark every video-only manifest
        // unresolvable, which is the wrong answer for a different reason.
        let has_audio = manifest.representations.iter().any(|r| r.is_audio);

        for rep in manifest.representations {
            let kind = match (rep.is_video, rep.is_audio) {
                (true, false) => MediaKind::VideoOnly,
                (false, true) => MediaKind::AudioOnly,
                _ => MediaKind::Unknown,
            };

            let mime = rep.mime.as_deref().unwrap_or("");

            // DASH segments are ISO-BMFF fragments — the joined output is a
            // regular `.mp4` — but the manifest only tells us that through the
            // MIME type, and some omit it. When it does, fall back to `Mp4` for a
            // segment plan rather than naming the result `.bin`.
            let container = if rep.segments.is_some() {
                match Container::from_mime(mime) {
                    Container::Other(_) => Container::Mp4,
                    other => other,
                }
            } else {
                Container::from_mime(mime)
            };

            media.formats.push(Format {
                id: rep.id.clone(),
                url: rep.base_url.clone(),
                container,
                kind,
                quality: rep.bandwidth.map(|b| Quality {
                    height: rep.height,
                    width: rep.width,
                    bitrate: Some(b),
                }),
                total_bytes: None,
                content_hash: None,
                mime: rep.mime.clone(),
                requires_mux: kind == MediaKind::VideoOnly && has_audio,
                segments: rep.segments,
            });
        }

        Ok(media)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::client::{Headers, RawResponse};
    use std::sync::Mutex;

    fn base() -> Url {
        Url::parse("https://cdn.example.invalid/dash/manifest.mpd").unwrap()
    }

    /// Hands out one scripted response, then fails loudly.
    struct Scripted {
        responses: Mutex<Vec<RawResponse>>,
    }

    impl Scripted {
        fn new(responses: Vec<RawResponse>) -> Self {
            Scripted {
                responses: Mutex::new(responses),
            }
        }
    }

    #[async_trait]
    impl crate::net::client::HttpClient for Scripted {
        async fn execute(
            &self,
            _: crate::net::client::HttpRequest,
        ) -> Result<RawResponse, crate::error::NetError> {
            let mut r = self.responses.lock().unwrap();
            assert!(!r.is_empty(), "ran out of scripted responses");
            Ok(r.remove(0))
        }
    }

    const SEGMENTED: &str = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011">
  <Period>
    <AdaptationSet mimeType="video/mp4" contentType="video">
      <Representation id="v-720" bandwidth="2000000" width="1280" height="720" codecs="avc1.4d401f">
        <BaseURL>video/</BaseURL>
        <SegmentList>
          <SegmentURL media="seg-1.m4s"/>
          <SegmentURL media="seg-2.m4s"/>
        </SegmentList>
      </Representation>
    </AdaptationSet>
    <AdaptationSet mimeType="audio/mp4" contentType="audio">
      <Representation id="a-128" bandwidth="128000" codecs="mp4a.40.2">
        <BaseURL>audio.mp4</BaseURL>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>
"#;

    #[test]
    fn parses_representations_with_an_explicit_segment_list() {
        let m = parse_manifest(SEGMENTED, &base()).unwrap();
        assert_eq!(m.representations.len(), 2);
        assert_eq!(
            m.profiles.as_deref(),
            Some("urn:mpeg:dash:profile:isoff-on-demand:2011")
        );

        let v = &m.representations[0];
        assert_eq!(v.id, "v-720");
        assert_eq!(v.bandwidth, Some(2_000_000));
        assert_eq!((v.width, v.height), (Some(1280), Some(720)));
        assert_eq!(v.codecs.as_deref(), Some("avc1.4d401f"));
        assert!(v.is_video);
        assert!(!v.is_audio);

        let segs = v.segments.as_ref().expect("segment plan");
        assert_eq!(segs.len(), 2);
        assert_eq!(
            segs[0].uri, "https://cdn.example.invalid/dash/video/seg-1.m4s",
            "BaseURL must resolve against the request URL"
        );
        assert_eq!(segs[1].index, 1);
    }

    #[test]
    fn a_single_file_representation_has_no_segment_plan() {
        let m = parse_manifest(SEGMENTED, &base()).unwrap();
        let a = &m.representations[1];
        assert!(a.is_audio);
        assert!(a.segments.is_none());
        assert_eq!(a.base_url, "https://cdn.example.invalid/dash/audio.mp4");
    }

    #[test]
    fn media_range_on_a_segment_becomes_a_byte_range() {
        let xml = r#"<MPD><Period><AdaptationSet contentType="video">
        <Representation id="v" bandwidth="1">
          <BaseURL>v.mp4</BaseURL>
          <SegmentList>
            <SegmentURL media="a.m4s" mediaRange="bytes=0-999"/>
          </SegmentList>
        </Representation></AdaptationSet></Period></MPD>"#;
        let m = parse_manifest(xml, &base()).unwrap();
        let seg = &m.representations[0].segments.as_ref().unwrap()[0];
        assert_eq!(
            seg.byte_range,
            Some(crate::net::range::ByteRange::closed(0, 1000))
        );
    }

    #[test]
    fn content_protection_is_refused_as_drm() {
        let xml = r#"<MPD><Period><AdaptationSet>
          <ContentProtection schemeIdUri="urn:mpeg:dash:mp4protection:2011"/>
        </AdaptationSet></Period></MPD>"#;
        assert!(matches!(
            parse_manifest(xml, &base()).unwrap_err(),
            ResolveError::DrmProtected
        ));
    }

    #[test]
    fn a_self_closing_representation_still_yields_a_format() {
        let xml = r#"<MPD><Period>
          <AdaptationSet contentType="video">
            <Representation id="v" bandwidth="800000" width="640" height="360"/>
          </AdaptationSet></Period></MPD>"#;
        let m = parse_manifest(xml, &base()).unwrap();
        assert_eq!(m.representations.len(), 1);
        assert_eq!(m.representations[0].bandwidth, Some(800_000));
        assert_eq!(m.representations[0].height, Some(360));
        assert!(m.representations[0].segments.is_none());
    }

    #[test]
    fn a_document_with_no_mpd_element_is_rejected() {
        assert!(matches!(
            parse_manifest("<html>nope</html>", &base()).unwrap_err(),
            ResolveError::Malformed { .. }
        ));
    }

    #[test]
    fn a_manifest_with_no_representations_is_rejected() {
        let xml = r#"<MPD><Period><AdaptationSet contentType="video"/></Period></MPD>"#;
        assert!(matches!(
            parse_manifest(xml, &base()).unwrap_err(),
            ResolveError::NoMedia { .. }
        ));
    }

    #[test]
    fn a_period_base_url_changes_where_segments_resolve() {
        // Real manifests encode per-period asset trees; getting the nesting
        // wrong silently fetches the wrong file.
        let xml = r#"<MPD><Period><BaseURL>p/</BaseURL><AdaptationSet contentType="video">
          <Representation id="v" bandwidth="1">
            <SegmentList><SegmentURL media="s.m4s"/></SegmentList>
          </Representation>
        </AdaptationSet></Period></MPD>"#;
        let m = parse_manifest(xml, &base()).unwrap();
        assert_eq!(
            m.representations[0].segments.as_ref().unwrap()[0].uri,
            "https://cdn.example.invalid/dash/p/s.m4s"
        );
    }

    #[test]
    fn a_representation_base_url_overrides_the_adaptation_set() {
        // The narrower scope has to win, or every Representation in a set would
        // share one asset directory.
        let xml = r#"<MPD><Period><AdaptationSet contentType="video">
          <BaseURL>set/</BaseURL>
          <Representation id="v" bandwidth="1">
            <BaseURL>rep/</BaseURL>
            <SegmentList><SegmentURL media="a.m4s"/></SegmentList>
          </Representation>
        </AdaptationSet></Period></MPD>"#;
        let m = parse_manifest(xml, &base()).unwrap();
        let uri = &m.representations[0].segments.as_ref().unwrap()[0].uri;
        // `set/` then `rep/` — the Representation scope is applied last.
        assert_eq!(uri, "https://cdn.example.invalid/dash/set/rep/a.m4s");
    }

    #[test]
    fn a_file_base_url_is_replaced_not_treated_as_a_directory() {
        // RFC 3986 drops the last path segment when resolving a relative
        // reference. A single-file BaseURL is therefore *not* a prefix you can
        // hang segment URLs off: `dash/v.mp4` + `a.m4s` is `dash/a.m4s`, not
        // `dash/v.mp4/a.m4s`. Manifests that intend nesting write a trailing
        // slash. Pinned here so the behaviour is never "corrected" by accident.
        let xml = r#"<MPD><Period><AdaptationSet contentType="video">
          <Representation id="v" bandwidth="1">
            <BaseURL>video/v.mp4</BaseURL>
            <SegmentList><SegmentURL media="a.m4s"/></SegmentList>
          </Representation>
        </AdaptationSet></Period></MPD>"#;
        let m = parse_manifest(xml, &base()).unwrap();
        assert_eq!(
            m.representations[0].base_url,
            "https://cdn.example.invalid/dash/video/v.mp4"
        );
        assert_eq!(
            m.representations[0].segments.as_ref().unwrap()[0].uri,
            "https://cdn.example.invalid/dash/video/a.m4s"
        );
    }

    #[test]
    fn entities_in_urls_are_decoded() {
        let xml = r#"<MPD><Period><AdaptationSet contentType="video">
          <Representation id="v" bandwidth="1">
            <BaseURL>a%26b/</BaseURL>
            <SegmentList><SegmentURL media="s?a=1&amp;b=2.m4s"/></SegmentList>
          </Representation>
        </AdaptationSet></Period></MPD>"#;
        let m = parse_manifest(xml, &base()).unwrap();
        let uri = &m.representations[0].segments.as_ref().unwrap()[0].uri;
        assert!(uri.contains("a=1&b=2"), "{uri}");
    }

    #[test]
    fn a_representation_without_an_id_gets_a_positional_one() {
        let xml = r#"<MPD><Period><AdaptationSet contentType="video">
          <Representation bandwidth="1"/><Representation bandwidth="2"/>
        </AdaptationSet></Period></MPD>"#;
        let m = parse_manifest(xml, &base()).unwrap();
        let ids: Vec<&str> = m.representations.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["r0", "r1"]);
    }

    /// An MPD offering video *and* audio.
    const VIDEO_AND_AUDIO: &str = r#"<MPD><Period>
      <AdaptationSet contentType="video"><Representation id="v" bandwidth="1"/></AdaptationSet>
      <AdaptationSet contentType="audio"><Representation id="a" bandwidth="2"/></AdaptationSet>
    </Period></MPD>"#;

    /// An MPD offering video only, with no audio track anywhere.
    const VIDEO_ONLY: &str = r#"<MPD><Period>
      <AdaptationSet contentType="video"><Representation id="v" bandwidth="1"/></AdaptationSet>
    </Period></MPD>"#;

    #[tokio::test]
    async fn a_video_only_manifest_is_downloadable() {
        // A presentation with no audio track needs no muxing step, so refusing it
        // would refuse perfectly good media.
        let c = Scripted::new(vec![RawResponse::full(
            200,
            VIDEO_ONLY,
            base().as_str(),
            Headers::new([(
                "Content-Type".to_string(),
                "application/dash+xml".to_string(),
            )]),
        )]);
        let media = DashResolver::new()
            .resolve(&c, &base())
            .await
            .expect("a video-only manifest resolves");
        assert_eq!(media.formats.len(), 1);
        assert!(!media.formats[0].requires_mux);
        assert!(media.best_standalone().is_some());
    }

    #[tokio::test]
    async fn a_video_rendition_needs_muxing_when_an_audio_track_exists() {
        // The opposite case: we must not hand the user a silent file and call it
        // the whole video.
        let c = Scripted::new(vec![RawResponse::full(
            200,
            VIDEO_AND_AUDIO,
            base().as_str(),
            Headers::new([(
                "Content-Type".to_string(),
                "application/dash+xml".to_string(),
            )]),
        )]);
        let media = DashResolver::new().resolve(&c, &base()).await.unwrap();

        let video = media
            .formats
            .iter()
            .find(|f| f.kind == MediaKind::VideoOnly)
            .expect("video rendition");
        assert!(video.requires_mux);

        // The audio rendition is standalone-playable on its own, so that is what
        // gets queued rather than nothing at all.
        let audio = media
            .formats
            .iter()
            .find(|f| f.kind == MediaKind::AudioOnly)
            .expect("audio rendition");
        assert!(!audio.requires_mux);
        assert_eq!(media.best_standalone().map(|f| f.id.as_str()), Some("a"));
    }

    #[test]
    fn resolver_claims_only_mpd_urls() {
        let r = DashResolver::new();
        assert!(r.matches(&Url::parse("https://x.invalid/a.mpd").unwrap()));
        assert!(r.matches(&Url::parse("https://x.invalid/A.MPD").unwrap()));
        assert!(!r.matches(&Url::parse("https://x.invalid/a.m3u8").unwrap()));
        assert!(!r.matches(&Url::parse("https://x.invalid/a.mp4").unwrap()));
    }
}
