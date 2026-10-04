//! HLS (RFC 8216) playlist parsing.
//!
//! Two shapes exist and both are supported:
//!
//! * **Master playlist** — `#EXT-X-STREAM-INF` variants, each a separate
//!   rendition the user may choose between.
//! * **Media playlist** — a single rendition as an ordered list of segments,
//!   which becomes a resumable transfer plan.
//!
//! # Encrypted playlists are refused
//!
//! Any `#EXT-X-KEY` with a `METHOD` other than `NONE` yields
//! [`ResolveError::DrmProtected`]. AES-128 HLS and its FairPlay/Widevine
//! relatives all require key retrieval we do not implement, so failing early and
//! clearly is better than handing the user a plan that stalls at segment zero.
//! See `docs/SCOPE.md` and ADR-0005.

use async_trait::async_trait;
use url::Url;

use crate::error::{NetError, ResolveError};
use crate::model::{Container, Format, Media, MediaKind, Quality, Segment};
use crate::net::client::{collect_limited, HttpClient, HttpRequest};
use crate::net::range::ByteRange;
use crate::net::CollectError;
use crate::resolve::direct::id_for;
use crate::resolve::{join, Resolver, MAX_DOCUMENT_BYTES};

/// Default `#EXT-X-TARGETDURATION` guess when the tag is absent.
///
/// Rare, and only used to size retry backoff, so a conservative default is
/// correct.
const DEFAULT_TARGET_DURATION_SECS: f64 = 10.0;

/// A single `#EXT-X-STREAM-INF` rendition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    /// Stable id, assigned by index.
    pub id: String,
    /// Absolute URI of the variant playlist.
    pub uri: String,
    /// Peak bitrate in bits per second, as declared by `BANDWIDTH`.
    pub bandwidth: Option<u64>,
    /// Average bitrate, as declared by `AVERAGE-BANDWIDTH`.
    pub average_bandwidth: Option<u64>,
    /// Pixel width, from `RESOLUTION`.
    pub width: Option<u16>,
    /// Pixel height, from `RESOLUTION`.
    pub height: Option<u16>,
    /// Codec list, verbatim from `CODECS`.
    pub codecs: Option<String>,
    /// Audio group referenced by `AUDIO`.
    pub audio_group: Option<String>,
    /// Subtitle group referenced by `SUBTITLES`.
    pub subtitle_group: Option<String>,
}

impl Variant {
    /// Quality derived from the variant's declared resolution and bitrate.
    pub fn quality(&self) -> Quality {
        Quality {
            height: self.height,
            width: self.width,
            bitrate: self.bandwidth.or(self.average_bandwidth),
        }
    }

    /// Whether this variant references a separate audio rendition, which makes
    /// the video track unusable on its own.
    pub fn is_video_only(&self) -> bool {
        self.audio_group.is_some()
    }
}

/// A parsed HLS playlist.
#[derive(Debug, Clone, PartialEq)]
pub struct Playlist {
    /// `#EXT-X-VERSION`, when declared.
    pub version: Option<u32>,
    /// Whether this is a master (variant) playlist rather than a media playlist.
    pub is_master: bool,
    /// Declared target segment duration, in seconds.
    pub target_duration_secs: Option<f64>,
    /// `#EXT-X-MEDIA-SEQUENCE` of the first segment.
    pub media_sequence: u64,
    /// Variants, present only on a master playlist.
    pub variants: Vec<Variant>,
    /// Segments, present only on a media playlist.
    pub segments: Vec<Segment>,
    /// Initialisation segment, from `#EXT-X-MAP`.
    pub init_segment: Option<Segment>,
    /// Sum of all `#EXTINF` durations.
    pub total_duration_secs: Option<f64>,
}

impl Playlist {
    /// What the joined segment bytes actually are.
    ///
    /// This is the question a user cares about when a file lands on disk, and it
    /// is not the same as "how was it delivered". A playlist with an
    /// `EXT-X-MAP` is fMP4 — an initialisation segment followed by media
    /// segments — which becomes a valid `.mp4` once joined. A playlist without
    /// one is MPEG-2 transport stream, which becomes `.ts`. Naming either by its
    /// playlist extension, or worse as `.bin`, leaves the user renaming files by
    /// hand.
    ///
    /// Lives here rather than in the resolver because three call sites need it:
    /// resolving a media playlist, expanding a variant playlist, and neither can
    /// afford to disagree about the answer.
    pub fn output_container(&self) -> Container {
        if self.init_segment.is_some() {
            Container::Mp4
        } else {
            Container::Mp2t
        }
    }

    /// The executable segment plan: init segment first, then every media
    /// segment, re-indexed from zero.
    ///
    /// The re-indexing is load-bearing rather than cosmetic. `#EXT-X-MAP` always
    /// carries index 0, so prepending it without renumbering yields
    /// `[0, 0, 1, 2, ...]`, and the transfer engine rejects any plan whose
    /// indices are not exactly `0..n` — that check is what keeps a resumed
    /// fragmented download from splicing bytes onto the wrong segments. Getting
    /// this wrong means every fMP4 HLS stream fails to download.
    pub fn segment_plan(&self) -> Vec<Segment> {
        let mut plan: Vec<Segment> = Vec::with_capacity(self.segments.len() + 1);
        if let Some(init) = &self.init_segment {
            plan.push(init.clone());
        }
        plan.extend(self.segments.iter().cloned());

        for (i, segment) in plan.iter_mut().enumerate() {
            segment.index = i as u32;
        }
        plan
    }
}

/// Parse a playlist, resolving every URI against `base`.
pub fn parse_playlist(text: &str, base: &Url) -> Result<Playlist, ResolveError> {
    if !text.trim_start().starts_with("#EXTM3U") {
        return Err(ResolveError::Malformed {
            url: base.to_string(),
            reason: "missing #EXTM3U tag; this is not an M3U8 playlist".into(),
        });
    }

    let malformed = |reason: String| ResolveError::Malformed {
        url: base.to_string(),
        reason,
    };

    let mut playlist = Playlist {
        version: None,
        is_master: false,
        target_duration_secs: None,
        media_sequence: 0,
        variants: Vec::new(),
        segments: Vec::new(),
        init_segment: None,
        total_duration_secs: None,
    };

    let mut pending_stream_inf: Option<Vec<(String, String)>> = None;
    let mut pending_duration: Option<f64> = None;
    let mut pending_byterange: Option<ByteRange> = None;
    let mut next_offset: u64 = 0;
    let mut duration_sum = 0.0f64;
    let mut saw_any_extinf = false;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(value) = line.strip_prefix("#EXT-X-VERSION:") {
            playlist.version = value.trim().parse().ok();
            continue;
        }
        if let Some(value) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            playlist.target_duration_secs = value.trim().parse().ok();
            continue;
        }
        if let Some(value) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            playlist.media_sequence = value.trim().parse().unwrap_or(0);
            continue;
        }
        if line == "#EXT-X-INDEPENDENT-SEGMENTS" {
            continue;
        }

        // Encryption is refused rather than partially supported.
        if let Some(value) = line.strip_prefix("#EXT-X-KEY:") {
            let attrs = parse_attributes(value).map_err(|e| malformed(e.to_string()))?;
            let method = attr(&attrs, "METHOD").unwrap_or_default();
            if !method.eq_ignore_ascii_case("NONE") {
                return Err(ResolveError::DrmProtected);
            }
            continue;
        }

        // DRM delivered via an SAMPLE-AES keyformat with no EXT-X-KEY.
        if line.starts_with("#EXT-X-SESSION-KEY") && crate::resolve::detect_drm_markers(line) {
            return Err(ResolveError::DrmProtected);
        }

        if let Some(value) = line.strip_prefix("#EXT-X-MAP:") {
            let attrs = parse_attributes(value).map_err(|e| malformed(e.to_string()))?;
            let uri = attr(&attrs, "URI")
                .ok_or_else(|| malformed("#EXT-X-MAP without a URI attribute".to_string()))?;
            let byte_range = attr(&attrs, "BYTERANGE")
                .map(|s| parse_byterange(s, 0))
                .transpose()
                .map_err(|e| malformed(e.to_string()))?;
            playlist.init_segment = Some(Segment {
                index: 0,
                uri: join(base, uri)?.to_string(),
                byte_range,
                duration_secs: None,
            });
            continue;
        }

        if let Some(value) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            playlist.is_master = true;
            pending_stream_inf =
                Some(parse_attributes(value).map_err(|e| malformed(e.to_string()))?);
            continue;
        }

        if let Some(value) = line.strip_prefix("#EXTINF:") {
            // `#EXTINF:<duration>,<optional title>`; the title is not useful here.
            let duration_str = value.split(',').next().unwrap_or("").trim();
            let duration = duration_str.parse::<f64>().ok();
            pending_duration = duration;
            if let Some(d) = duration {
                duration_sum += d;
                saw_any_extinf = true;
            }
            continue;
        }

        if let Some(value) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            // An omitted offset continues from the end of the previous segment in
            // the same resource, which is why we thread `next_offset` through.
            pending_byterange =
                Some(parse_byterange(value, next_offset).map_err(|e| malformed(e.to_string()))?);
            continue;
        }

        if line.starts_with('#') {
            // Any other tag we do not implement. Ignoring is correct: tags such
            // as `#EXT-X-PLAYLIST-TYPE` and `#EXT-X-DISCONTINUITY` are advisory.
            continue;
        }

        // A bare line is a URI, and the tag above it decides what it means.
        let uri = match pending_stream_inf.take() {
            Some(attrs) => {
                let bandwidth = attr(&attrs, "BANDWIDTH").and_then(|v| v.parse().ok());
                let (width, height) = match attr(&attrs, "RESOLUTION") {
                    Some(res) => parse_resolution(res).ok().unzip(),
                    None => (None, None),
                };
                playlist.variants.push(Variant {
                    id: format!("v{}", playlist.variants.len()),
                    uri: join(base, line)?.to_string(),
                    bandwidth,
                    average_bandwidth: attr(&attrs, "AVERAGE-BANDWIDTH")
                        .and_then(|v| v.parse().ok()),
                    width,
                    height,
                    codecs: attr(&attrs, "CODECS").map(str::to_string),
                    audio_group: attr(&attrs, "AUDIO").map(str::to_string),
                    subtitle_group: attr(&attrs, "SUBTITLES").map(str::to_string),
                });
                continue;
            }
            None => line,
        };

        let range = pending_byterange.take();
        if let Some(r) = range {
            next_offset = r.end.unwrap_or(next_offset);
        }

        playlist.segments.push(Segment {
            index: playlist.segments.len() as u32,
            uri: join(base, uri)?.to_string(),
            byte_range: range,
            duration_secs: pending_duration.take(),
        });
    }

    if playlist.target_duration_secs.is_none() {
        playlist.target_duration_secs = Some(DEFAULT_TARGET_DURATION_SECS);
    }
    if saw_any_extinf {
        playlist.total_duration_secs = Some(duration_sum);
    }

    if playlist.variants.is_empty() && playlist.segments.is_empty() {
        return Err(ResolveError::NoMedia {
            url: base.to_string(),
        });
    }

    Ok(playlist)
}

/// Parse `WIDTHxHEIGHT`.
fn parse_resolution(s: &str) -> Result<(u16, u16), String> {
    let (w, h) = s
        .trim()
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("malformed RESOLUTION {s:?}"))?;
    Ok((
        w.trim()
            .parse()
            .map_err(|_| format!("bad width in {s:?}"))?,
        h.trim()
            .parse()
            .map_err(|_| format!("bad height in {s:?}"))?,
    ))
}

/// Parse `length[@offset]`, resolving an omitted offset against `implicit_offset`.
fn parse_byterange(s: &str, implicit_offset: u64) -> Result<ByteRange, String> {
    let spec = s.trim();
    let (len_str, off_str) = match spec.split_once('@') {
        Some((l, o)) => (l, Some(o)),
        None => (spec, None),
    };

    let len: u64 = len_str
        .trim()
        .parse()
        .map_err(|_| format!("malformed EXT-X-BYTERANGE length in {spec:?}"))?;

    let start = match off_str {
        Some(o) => o
            .trim()
            .parse()
            .map_err(|_| format!("malformed EXT-X-BYTERANGE offset in {spec:?}"))?,
        None => implicit_offset,
    };

    Ok(ByteRange::closed(start, start + len))
}

/// Parse an HLS attribute list: `KEY=VALUE,KEY="quoted,value"`.
pub fn parse_attributes(input: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in input.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            ',' if !in_quotes => {
                out.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    if in_quotes {
        return Err("unterminated quoted attribute value".into());
    }
    out.push(current);

    Ok(out
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .map(|pair| {
            let pair = pair.trim();
            match pair.split_once('=') {
                Some((k, v)) => (
                    k.trim().to_ascii_uppercase(),
                    strip_quotes(v.trim()).to_string(),
                ),
                // A bare token, e.g. `CODECS` with no value.
                None => (pair.to_ascii_uppercase(), String::new()),
            }
        })
        .collect())
}

fn strip_quotes(v: &str) -> &str {
    v.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(v)
}

/// Case-insensitive attribute lookup.
pub fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
}

/// Resolves HLS playlist URLs.
#[derive(Debug, Clone, Copy, Default)]
pub struct HlsResolver;

impl HlsResolver {
    /// Create the resolver.
    pub fn new() -> Self {
        Self
    }
}

impl HlsResolver {
    /// Fetch and parse the playlist at `url`.
    pub async fn fetch(
        &self,
        client: &dyn HttpClient,
        url: &Url,
    ) -> Result<Playlist, ResolveError> {
        let text = fetch_text(client, url).await?;
        parse_playlist(&text, url)
    }
}

pub(crate) async fn fetch_text(client: &dyn HttpClient, url: &Url) -> Result<String, ResolveError> {
    let resp = match client.execute(HttpRequest::get(url.as_str())).await {
        Ok(r) => r,
        Err(NetError::Status {
            status: 401 | 403, ..
        }) => return Err(ResolveError::AuthRequired),
        Err(e) => return Err(map_net(e)),
    };

    let bytes = match collect_limited(resp, MAX_DOCUMENT_BYTES).await {
        Ok(b) => b,
        Err(CollectError::TooLarge { limit }) => {
            return Err(ResolveError::ResponseTooLarge {
                url: url.to_string(),
                limit,
            })
        }
        Err(CollectError::Net(NetError::Status {
            status: 401 | 403, ..
        })) => return Err(ResolveError::AuthRequired),
        Err(CollectError::Net(e)) => return Err(map_net(e)),
    };

    String::from_utf8(bytes).map_err(|_| ResolveError::Malformed {
        url: url.to_string(),
        reason: "document is not valid UTF-8".into(),
    })
}

pub(crate) fn map_net(e: NetError) -> ResolveError {
    match e {
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

#[async_trait]
impl Resolver for HlsResolver {
    fn matches(&self, url: &Url) -> bool {
        if !matches!(url.scheme(), "http" | "https") {
            return false;
        }
        url.path().to_ascii_lowercase().ends_with(".m3u8")
    }

    fn name(&self) -> &'static str {
        "hls"
    }

    async fn resolve(&self, client: &dyn HttpClient, url: &Url) -> Result<Media, ResolveError> {
        let playlist = self.fetch(client, url).await?;

        let mut media = Media::new(id_for(url), "hls", url.as_str());
        media.duration_secs = playlist.total_duration_secs.map(|d| d.round() as u64);

        if playlist.is_master {
            for v in &playlist.variants {
                media.formats.push(Format {
                    id: v.id.clone(),
                    url: v.uri.clone(),
                    container: Container::Hls,
                    kind: if v.audio_group.is_some() {
                        MediaKind::VideoOnly
                    } else {
                        MediaKind::VideoWithAudio
                    },
                    quality: Some(v.quality()),
                    total_bytes: None,
                    content_hash: None,
                    mime: Some("application/vnd.apple.mpegurl".into()),
                    requires_mux: v.audio_group.is_some(),
                    // A variant playlist is expanded lazily: each variant is
                    // itself a media playlist, and fetching them all up front
                    // would mean one request per rendition for something the
                    // user may never choose.
                    segments: None,
                });
            }
        } else {
            media.formats.push(Format {
                id: "0".into(),
                url: url.to_string(),
                container: playlist.output_container(),
                kind: MediaKind::VideoWithAudio,
                quality: None,
                total_bytes: None,
                content_hash: None,
                mime: Some("application/vnd.apple.mpegurl".into()),
                requires_mux: false,
                segments: Some(playlist.segment_plan()),
            });
        }

        Ok(media)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://cdn.example.invalid/media/hls/master.m3u8").unwrap()
    }

    const MASTER: &str = r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-INDEPENDENT-SEGMENTS
#EXT-X-STREAM-INF:BANDWIDTH=1280000,AVERAGE-BANDWIDTH=1000000,RESOLUTION=640x360,CODECS="avc1.4d401e,mp4a.40.2",AUDIO="aud"
360p.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=5120000,RESOLUTION=1920x1080,CODECS="avc1.640028,mp4a.40.2"
1080p.m3u8
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="aud",NAME="English",URI="audio/en.m3u8"
"#;

    #[test]
    fn parses_a_master_playlist() {
        let p = parse_playlist(MASTER, &base()).unwrap();
        assert!(p.is_master);
        assert_eq!(p.version, Some(6));
        assert_eq!(p.variants.len(), 2);

        let low = &p.variants[0];
        assert_eq!(low.bandwidth, Some(1_280_000));
        assert_eq!(low.average_bandwidth, Some(1_000_000));
        assert_eq!((low.width, low.height), (Some(640), Some(360)));
        assert_eq!(
            low.codecs.as_deref(),
            Some("avc1.4d401e,mp4a.40.2"),
            "a comma inside a quoted CODECS value must not split attributes"
        );
        assert_eq!(low.audio_group.as_deref(), Some("aud"));
        assert_eq!(low.uri, "https://cdn.example.invalid/media/hls/360p.m3u8");

        let high = &p.variants[1];
        assert_eq!((high.width, high.height), (Some(1920), Some(1080)));
        assert!(high.audio_group.is_none());
    }

    #[test]
    fn a_variant_with_an_audio_group_is_video_only() {
        let p = parse_playlist(MASTER, &base()).unwrap();
        assert!(p.variants[0].is_video_only());
        assert!(!p.variants[1].is_video_only());
    }

    #[test]
    fn parses_a_media_playlist_with_segments() {
        let text = r#"#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:6
#EXT-X-MEDIA-SEQUENCE:100
#EXTINF:6.000,
seg0.ts
#EXTINF:6.000,
seg1.ts
#EXTINF:4.500,
seg2.ts
#EXT-X-ENDLIST
"#;
        let p = parse_playlist(text, &base()).unwrap();
        assert!(!p.is_master);
        assert_eq!(p.segments.len(), 3);
        assert_eq!(p.media_sequence, 100);
        assert_eq!(p.target_duration_secs, Some(6.0));
        assert_eq!(p.total_duration_secs, Some(16.5));

        assert_eq!(p.segments[0].index, 0);
        assert_eq!(
            p.segments[2].uri,
            "https://cdn.example.invalid/media/hls/seg2.ts"
        );
        assert_eq!(p.segments[2].duration_secs, Some(4.5));
    }

    #[test]
    fn implied_byterange_offsets_accumulate() {
        // `#EXT-X-BYTERANGE:1000` with no offset continues from the end of the
        // previous segment in the same resource. Getting this wrong silently
        // fetches the same bytes twice and corrupts the file.
        let text = r#"#EXTM3U
#EXTINF:6.0,
#EXT-X-BYTERANGE:1000
all.ts
#EXTINF:6.0,
#EXT-X-BYTERANGE:1000
all.ts
#EXTINF:6.0,
#EXT-X-BYTERANGE:500@2000
all.ts
"#;
        let p = parse_playlist(text, &base()).unwrap();
        assert_eq!(p.segments[0].byte_range, Some(ByteRange::closed(0, 1000)));
        assert_eq!(
            p.segments[1].byte_range,
            Some(ByteRange::closed(1000, 2000))
        );
        assert_eq!(
            p.segments[2].byte_range,
            Some(ByteRange::closed(2000, 2500))
        );
    }

    #[test]
    fn explicit_byterange_offsets_are_honoured() {
        let p = parse_byterange("512@1024", 0).unwrap();
        assert_eq!(p, ByteRange::closed(1024, 1536));
    }

    #[test]
    fn init_segments_are_prepended_to_the_plan() {
        let text = r#"#EXTM3U
#EXT-X-MAP:URI="init.mp4",BYTERANGE="720@0"
#EXTINF:4.0,
seg0.m4s
#EXTINF:4.0,
seg1.m4s
"#;
        let p = parse_playlist(text, &base()).unwrap();
        let init = p.init_segment.as_ref().expect("init segment");
        assert_eq!(init.uri, "https://cdn.example.invalid/media/hls/init.mp4");
        assert_eq!(init.byte_range, Some(ByteRange::closed(0, 720)));

        // The executable plan must be exactly `0..n`. `EXT-X-MAP` carries index
        // 0 of its own, so prepending it without renumbering yields `[0, 0, 1]`
        // and the engine rejects the plan — every fMP4 HLS stream would fail.
        let plan = p.segment_plan();
        assert_eq!(plan.len(), 3);
        assert_eq!(
            plan.iter().map(|s| s.index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(
            plan[0].uri,
            "https://cdn.example.invalid/media/hls/init.mp4"
        );
        assert_eq!(
            plan[1].uri,
            "https://cdn.example.invalid/media/hls/seg0.m4s"
        );
        assert_eq!(
            plan[2].uri,
            "https://cdn.example.invalid/media/hls/seg1.m4s"
        );
    }

    #[test]
    fn a_plan_without_an_init_segment_is_still_contiguous() {
        let p = parse_playlist(
            "#EXTM3U\n#EXTINF:4.0,\na.m4s\n#EXTINF:4.0,\nb.m4s\n",
            &base(),
        )
        .unwrap();
        assert_eq!(
            p.segment_plan().iter().map(|s| s.index).collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn encrypted_playlists_are_refused_as_drm() {
        let aes = r#"#EXTM3U
#EXT-X-VERSION:3
#EXT-X-KEY:METHOD=AES-128,URI="skd://key",IV=0x00
#EXTINF:6.0,
seg0.ts
"#;
        let err = parse_playlist(aes, &base()).unwrap_err();
        assert!(matches!(err, ResolveError::DrmProtected), "{err}");

        let sample_aes = r#"#EXTM3U
#EXT-X-SESSION-KEY:METHOD=SAMPLE-AES,KEYFORMAT="com.apple.streamingkeydelivery"
"#;
        assert!(matches!(
            parse_playlist(sample_aes, &base()).unwrap_err(),
            ResolveError::DrmProtected
        ));
    }

    #[test]
    fn method_none_is_not_treated_as_encryption() {
        let text = r#"#EXTM3U
#EXT-X-KEY:METHOD=NONE
#EXTINF:6.0,
seg0.ts
"#;
        assert!(parse_playlist(text, &base()).is_ok());
    }

    #[test]
    fn a_non_playlist_document_is_rejected() {
        let err = parse_playlist("<html>nope</html>", &base()).unwrap_err();
        assert!(matches!(err, ResolveError::Malformed { .. }), "{err}");
    }

    #[test]
    fn a_playlist_with_no_media_is_rejected() {
        let err = parse_playlist("#EXTM3U\n#EXT-X-VERSION:3\n", &base()).unwrap_err();
        assert!(matches!(err, ResolveError::NoMedia { .. }), "{err}");
    }

    #[test]
    fn unknown_tags_are_ignored_rather_than_failing() {
        // Discontinuity, playlist type, program date-time and friends are
        // advisory. Failing on them would reject many valid real-world
        // playlists.
        let text = r#"#EXTM3U
#EXT-X-VERSION:7
#EXT-X-PLAYLIST-TYPE:VOD
#EXT-X-INDEPENDENT-SEGMENTS
#EXT-X-DISCONTINUITY
#EXT-X-PROGRAM-DATE-TIME:2026-10-01T00:00:00Z
#EXTINF:6.0,
seg0.ts
#EXT-X-ENDLIST
"#;
        let p = parse_playlist(text, &base()).unwrap();
        assert_eq!(p.segments.len(), 1);
    }

    #[test]
    fn attribute_parsing_handles_quoted_commas_and_bare_tokens() {
        let attrs = parse_attributes(r#"A="x,y",B=1,C"#).unwrap();
        assert_eq!(attr(&attrs, "A"), Some("x,y"));
        assert_eq!(attr(&attrs, "B"), Some("1"));
        // A bare token is stored with an empty value, and an empty value reads
        // back as absent. `AUDIO=""` must not be mistaken for a named audio
        // group, which would mark every variant video-only.
        assert_eq!(attr(&attrs, "C"), None);
    }

    #[test]
    fn attribute_parsing_rejects_an_unterminated_quote() {
        assert!(parse_attributes(r#"A="unterminated"#).is_err());
    }

    #[test]
    fn malformed_resolution_is_rejected_rather_than_defaulting() {
        assert!(parse_resolution("1920").is_err());
        assert!(parse_resolution("axb").is_err());
        assert_eq!(parse_resolution("1920X1080").unwrap(), (1920, 1080));
    }

    #[test]
    fn resolution_matching_is_equivalent() {
        // Real playlists use both cases; a case-sensitive split would drop the
        // quality of an entire rendition.
        assert_eq!(parse_resolution("1280x720").unwrap(), (1280, 720));
        assert_eq!(parse_resolution("1280X720").unwrap(), (1280, 720));
    }

    #[test]
    fn resolver_claims_only_m3u8_urls() {
        let r = HlsResolver::new();
        assert!(r.matches(&Url::parse("https://x.invalid/a.m3u8").unwrap()));
        assert!(r.matches(&Url::parse("https://x.invalid/A.M3U8").unwrap()));
        assert!(!r.matches(&Url::parse("https://x.invalid/a.mp4").unwrap()));
        assert!(!r.matches(&Url::parse("file:///tmp/a.m3u8").unwrap()));
    }

    #[test]
    fn the_query_string_cannot_change_what_a_url_is() {
        // Only the path decides. Signed CDN manifests routinely carry query
        // strings — and those strings themselves end in `.mp4`, `.ts`, or any
        // other extension you care to name. Matching on the whole URL would
        // misclassify a real manifest and then hand the user the wrong file.
        let r = HlsResolver::new();
        assert!(r.matches(&Url::parse("https://x.invalid/master.m3u8?x=1.mp4").unwrap()));
        assert!(r.matches(
            &Url::parse("https://x.invalid/master.m3u8?sig=abc&file=a.ts&x=.webm").unwrap()
        ));
        assert!(!r.matches(&Url::parse("https://x.invalid/clip.mp4?next=/a.m3u8").unwrap()));
    }

    #[test]
    fn a_master_playlist_yields_one_format_per_variant() {
        let p = parse_playlist(MASTER, &base()).unwrap();
        assert_eq!(p.variants.len(), 2);
        // Lazy expansion: a variant's segment list is fetched only when chosen.
        assert!(p.variants.iter().all(|v| v.uri.ends_with(".m3u8")));
    }

    #[test]
    fn the_output_extension_describes_the_bytes_not_the_playlist() {
        // fMP4 (`EXT-X-MAP` present) joins into a real `.mp4`; without one the
        // segments are transport stream and join into `.ts`. Naming these
        // `.m3u8`, or worse `.bin`, means the user renames files by hand.
        let fmp4 = parse_playlist(
            "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MAP:URI=\"init.mp4\"\n\
             #EXTINF:4.0,\ns0.m4s\n#EXT-X-ENDLIST\n",
            &base(),
        )
        .unwrap();
        assert_eq!(fmp4.output_container(), Container::Mp4);
        assert_eq!(fmp4.output_container().extension(), "mp4");

        let ts = parse_playlist(
            "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.0,\ns0.ts\n#EXT-X-ENDLIST\n",
            &base(),
        )
        .unwrap();
        assert_eq!(ts.output_container(), Container::Mp2t);
        assert_eq!(ts.output_container().extension(), "ts");
    }

    #[test]
    fn crlf_line_endings_are_tolerated() {
        let text = MASTER.replace('\n', "\r\n");
        assert!(parse_playlist(&text, &base()).is_ok());
    }
}
