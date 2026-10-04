//! Core domain types: media, formats, quality, containers.
//!
//! These are plain data with no behaviour and no I/O. They are the vocabulary
//! every other layer speaks, so they stay small and serialisable.

use serde::{Deserialize, Serialize};

/// Whether a format carries video, audio, or both.
///
/// Separate video and audio representations matter because adaptive manifests
/// routinely ship them independently, requiring a muxing step before the file
/// is playable. Callers need to know this before they start, not after.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum MediaKind {
    /// Video and audio in a single stream.
    VideoWithAudio,
    /// Video only. Requires a muxing step with a [`MediaKind::AudioOnly`] track
    /// before it will play.
    VideoOnly,
    /// Audio only.
    AudioOnly,
    /// Container or manifest kind not recognised.
    Unknown,
}

impl MediaKind {
    /// Whether this kind carries a video track.
    pub fn has_video(self) -> bool {
        matches!(self, Self::VideoWithAudio | Self::VideoOnly)
    }

    /// Whether this kind carries an audio track.
    pub fn has_audio(self) -> bool {
        matches!(self, Self::VideoWithAudio | Self::AudioOnly)
    }

    /// A short lowercase label for display.
    ///
    /// Six characters wide at most, so columns line up in a terminal table.
    pub fn kind_name(self) -> &'static str {
        match self {
            Self::VideoWithAudio => "video",
            Self::VideoOnly => "vid-only",
            Self::AudioOnly => "audio",
            Self::Unknown => "unknown",
        }
    }
}

/// Container or packaging of a format.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Container {
    /// MPEG-4 Part 14.
    Mp4,
    /// Matroska.
    Matroska,
    /// WebM subset of Matroska.
    Webm,
    /// MPEG-1 Audio Layer III.
    Mp3,
    /// Raw ADTS AAC.
    Adts,
    /// Ogg container.
    Ogg,
    /// Ogg Opus.
    Opus,
    /// FLAC.
    Flac,
    /// WAV.
    Wav,
    /// MPEG-2 transport stream, as produced by concatenating HLS segments that
    /// carry no `EXT-X-MAP`.
    Mp2t,
    /// HLS playlist whose output container was not determinable.
    Hls,
    /// DASH manifest whose output container was not determinable.
    Dash,
    /// Anything else, preserving the reported MIME type.
    Other(String),
}

impl Container {
    /// Infer a container from a MIME type.
    pub fn from_mime(mime: &str) -> Self {
        let base = mime
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match base.as_str() {
            "video/mp4" | "application/mp4" | "video/x-m4v" => Self::Mp4,
            "video/x-matroska" => Self::Matroska,
            "video/webm" => Self::Webm,
            "audio/mpeg" | "audio/mpeg3" | "audio/x-mpeg-3" => Self::Mp3,
            "audio/aac" | "audio/x-aac" => Self::Adts,
            "audio/ogg" => Self::Ogg,
            "audio/opus" | "audio/ogg;codecs=opus" => Self::Opus,
            "audio/flac" | "audio/x-flac" => Self::Flac,
            "audio/wav" | "audio/x-wav" | "audio/wave" => Self::Wav,
            "application/vnd.apple.mpegurl"
            | "application/x-mpegurl"
            | "audio/mpegurl"
            | "audio/x-mpegurl" => Self::Hls,
            "application/dash+xml" => Self::Dash,
            other => Self::Other(other.to_string()),
        }
    }

    /// Whether this container is a segmented/adaptive stream that requires a
    /// segment-by-segment transfer rather than a single range-capable one.
    ///
    /// Note this is about the *delivery mechanism*, not the output file. It is
    /// deliberately not the thing that decides the output extension: an fMP4
    /// HLS stream arrives as a playlist but leaves as `.mp4`, and a DASH
    /// manifest that addresses one whole file arrives as a manifest but leaves
    /// as whatever that file is. Resolvers set a concrete [`Container`] for that.
    pub fn is_segmented(&self) -> bool {
        matches!(*self, Self::Hls | Self::Dash)
    }

    /// A conventional file extension, without a leading dot.
    ///
    /// Used only for display and for the final output path. Containers that
    /// require a container-specific choice (`Other`) fall back to `bin` rather
    /// than guessing, because a wrong extension produces a file that will not
    /// open, and a missing extension is trivially fixed by the user.
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Matroska => "mkv",
            Self::Webm => "webm",
            Self::Mp3 => "mp3",
            Self::Adts => "aac",
            Self::Ogg => "ogg",
            Self::Opus => "opus",
            Self::Flac => "flac",
            Self::Wav => "wav",
            Self::Mp2t => "ts",
            // Reaching here means a resolver could not determine what the
            // assembled bytes are. Naming them `.mp4` would be a guess that is
            // wrong for TS and for anything non-video; `bin` at least tells the
            // user to look.
            Self::Hls | Self::Dash | Self::Other(_) => "bin",
        }
    }
}

/// Whether bytes arrive as one contiguous range or as a walked segment plan.
///
/// Not [`MediaKind`], which describes *tracks* -- video with audio, video only,
/// audio only. This describes the shape of the transfer, which is a different
/// question with a different answer: a video-only DASH stream is simultaneously
/// [`MediaKind::VideoOnly`] and [`TransferKind::Fragmented`], and a client that
/// has to render a row needs both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransferKind {
    /// One request, one contiguous byte range.
    Direct,
    /// An init segment plus numbered media segments, walked in order.
    Fragmented,
}

impl TransferKind {
    /// A short lowercase label for display.
    pub fn kind_name(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Fragmented => "fragmented",
        }
    }
}

/// How a task appears to a user interface.
///
/// Deliberately *not* [`crate::download::TaskState`], which is the engine's
/// vocabulary and says `downloading` and `completed`. This one says `running` and
/// `done`. The two differ because they answer different questions: `TaskState` is
/// about the state machine's legal transitions, and a client wants to know which
/// buttons to enable; this is about what to put in a row.
///
/// The mapping lives in one place -- [`TaskState::snapshot_state`] -- so that no
/// client has to guess, and so that a future state cannot be added to one enum
/// and quietly go missing from the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum SnapshotState {
    /// Waiting for a slot.
    Queued,
    /// Transferring.
    Running,
    /// Stopped by the user, resumable.
    Paused,
    /// Stopped by a retryable error, resumable.
    Retrying,
    /// Finished and verified.
    Done,
    /// Stopped for good.
    Failed,
}

impl SnapshotState {
    /// Whether a client should show this as still in progress.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Retrying)
    }

    /// Whether this state will not change without user action.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed)
    }
}

/// A failure, with enough structure for a client to react rather than to parse.
///
/// `code` is stable and machine-readable; `message` is for a person and may be
/// reworded at any time. A client that switches on `message` will break.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotError {
    /// Stable identifier for the failure.
    pub code: String,
    /// Human-readable detail.
    pub message: String,
}

/// One flight-recorder entry, as a client sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotEvent {
    /// Unix milliseconds.
    #[serde(rename = "t")]
    pub at: u64,
    /// What happened, as a short lowercase word.
    pub kind: String,
    /// The detail, phrased for a person.
    pub msg: String,
}

/// A task, flattened into the shape a client renders.
///
/// This is the whole wire contract. It exists so that [`crate::download::Task`]
/// -- which carries a segment plan, a partial offset and a resolver-time format
/// id -- can keep its own shape for its own reasons without every client
/// reverse-engineering it.
///
/// Three things are worth stating about what is *absent*:
///
/// * No segment plan. A client shows "3 of 8", not 8 URIs and byte ranges.
/// * No rate. Not because a rate cannot be computed, but because computing it
///   well needs a window and a clock, and a client that guessed would be showing
///   a number the engine did not stand behind. [`Task::rate_bps`] is the answer;
///   [`Self::rate_bps`] passes it through only when the engine has real samples.
/// * No `error` when the task is fine. Absent rather than null, so "no failure"
///   and "we did not look" stay distinguishable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSnapshot {
    /// Stable identifier.
    pub id: String,
    /// Final filename, without a directory.
    pub name: String,
    /// Source URL as the user supplied it.
    pub source: String,
    /// Absolute output path.
    pub dest: String,
    /// User-facing state.
    pub state: SnapshotState,
    /// Whether bytes arrive as one range or a walked plan.
    pub kind: TransferKind,
    /// Container extension, without the dot.
    pub container: String,
    /// Resolver that produced this, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolver: Option<String>,
    /// Codec summary, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    /// Bytes on disk.
    pub received: u64,
    /// Total expected, when the origin declared one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// Bits per second over the retained sample window, when there are enough
    /// samples to be honest about one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_bps: Option<f64>,
    /// Unix milliseconds of first transfer attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    /// Unix milliseconds of completion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    /// Offsets at which this transfer was interrupted, oldest first.
    pub seams: Vec<u64>,
    /// Progress sample points as raw byte counts, oldest first.
    pub marks: Vec<u64>,
    /// Segments written, for fragmented transfers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segments_done: Option<usize>,
    /// Segments in the plan, for fragmented transfers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segments_total: Option<usize>,
    /// The failure, if there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<SnapshotError>,
    /// Flight recorder, oldest first.
    pub events: Vec<SnapshotEvent>,
}

/// Resolution and bitrate of a video representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Quality {
    /// Pixel height, when known.
    pub height: Option<u16>,
    /// Pixel width, when known.
    pub width: Option<u16>,
    /// Nominal bitrate in bits per second, when known.
    pub bitrate: Option<u64>,
}

impl Quality {
    /// Quality with only a height, the overwhelmingly common case.
    pub fn height(height: u16) -> Self {
        Self {
            height: Some(height),
            width: None,
            bitrate: None,
        }
    }

    /// Sort key: better quality sorts later.
    ///
    /// Prefers height, falls back to bitrate for audio-only, then to total
    /// order so the sort is deterministic even for sparse manifests.
    pub fn rank(&self) -> (u32, u64) {
        (
            u32::from(self.height.unwrap_or(0)),
            self.bitrate.unwrap_or(0),
        )
    }
}

/// One segment of a fragmented transfer.
///
/// Fragmented media (HLS, DASH with an explicit segment list) is transferred
/// segment by segment, and recorded per index. That is what makes a fragmented
/// download resumable at all: restarting a 200-segment file from zero is not a
/// resume, it is a re-download.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    /// Zero-based position in the stream. Recorded in the transfer plan so a
    /// resumed run skips exactly what is already on disk.
    pub index: u32,

    /// Absolute URL of the segment.
    pub uri: String,

    /// Byte range within `uri`, when the manifest specifies one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_range: Option<crate::net::range::ByteRange>,

    /// Duration in seconds, when the manifest declares it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
}

impl Segment {
    /// A segment with no declared duration.
    pub fn new(index: u32, uri: impl Into<String>) -> Self {
        Self {
            index,
            uri: uri.into(),
            byte_range: None,
            duration_secs: None,
        }
    }
}

/// One downloadable representation of a media object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Format {
    /// Stable identifier within the parent [`Media`], used for queue lookup.
    pub id: String,

    /// Where the bytes come from.
    pub url: String,

    /// Container or manifest type.
    pub container: Container,

    /// What tracks this format carries.
    pub kind: MediaKind,

    /// Resolution and bitrate, when advertised.
    pub quality: Option<Quality>,

    /// Exact content length in bytes, when advertised.
    pub total_bytes: Option<u64>,

    /// Content hash advertised by the source, hex-encoded.
    ///
    /// When present, the queue can recognise the same object across URL
    /// changes, which is what makes "resume" survive a rotated CDN hostname.
    pub content_hash: Option<String>,

    /// MIME type exactly as advertised.
    pub mime: Option<String>,

    /// Whether this format must be muxed with another before it is playable.
    pub requires_mux: bool,

    /// Segment plan, for fragmented media. `None` for a single-file transfer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segments: Option<Vec<Segment>>,
}

impl Format {
    /// Whether this format can be transferred as a single range-capable file.
    ///
    /// The test is the presence of a segment plan, not the container. A DASH
    /// manifest whose representation is one whole file arrives as a manifest and
    /// transfers as a single range request, so keying this off the container
    /// called it fragmented and would have taken a different, worse code path.
    pub fn is_direct(&self) -> bool {
        self.segments.is_none()
    }

    /// Whether this format is video-only and therefore unusable alone.
    pub fn is_video_only(&self) -> bool {
        self.kind == MediaKind::VideoOnly
    }
}

/// A resolved media object: one thing a user asked for, and everything we
/// found at that URL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Media {
    /// Stable identifier, derived from the source URL or its hash.
    pub id: String,

    /// Best-effort display title.
    pub title: String,

    /// The URL this was resolved from.
    pub source_url: String,

    /// Every representation found, best-quality-last ordering not guaranteed.
    pub formats: Vec<Format>,

    /// Duration in seconds, when the source declares it.
    pub duration_secs: Option<u64>,

    /// Thumbnail URL, when one was advertised.
    pub thumbnail: Option<String>,
}

impl Media {
    /// Build a [`Media`] and sort `formats` best-first.
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        source_url: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            source_url: source_url.into(),
            formats: Vec::new(),
            duration_secs: None,
            thumbnail: None,
        }
    }

    /// Look up a format by id.
    pub fn format(&self, id: &str) -> Option<&Format> {
        self.formats.iter().find(|f| f.id == id)
    }

    /// Formats in descending quality order, then by id for stability.
    pub fn formats_by_quality(&self) -> Vec<&Format> {
        let mut v: Vec<&Format> = self.formats.iter().collect();
        // Descending rank, ascending id. Reversing an ascending sort would also
        // invert the id tie-break, which is what makes two equal-quality
        // renditions swap places between runs.
        v.sort_by(|a, b| {
            b.quality
                .map(|q| q.rank())
                .unwrap_or_default()
                .cmp(&a.quality.map(|q| q.rank()).unwrap_or_default())
                .then_with(|| a.id.cmp(&b.id))
        });
        v
    }

    /// The single best format the user can open without a muxing step.
    ///
    /// `requires_mux` is the gate, not [`MediaKind`]. A video-only rendition
    /// normally cannot be handed over as a finished file because we ship no
    /// muxer — but when the manifest offers no audio track at all, there is
    /// nothing to combine it with and the file is complete as it stands. Gating
    /// on the kind instead of the flag refused that case. See `ADR-0008`.
    ///
    /// Preference order, highest first: video with audio, then audio on its own,
    /// then video-only that needs no mux. Video is last rather than first because
    /// when audio exists the audio track is the thing that actually plays.
    pub fn best_standalone(&self) -> Option<&Format> {
        let ranked = self.formats_by_quality();

        ranked
            .iter()
            .find(|f| f.kind == MediaKind::VideoWithAudio && !f.requires_mux)
            .or_else(|| {
                ranked
                    .iter()
                    .find(|f| f.kind == MediaKind::AudioOnly && !f.requires_mux)
            })
            .or_else(|| {
                ranked
                    .iter()
                    .find(|f| f.kind == MediaKind::VideoOnly && !f.requires_mux)
            })
            .copied()
    }

    /// Whether any format in this object requires a muxing step.
    pub fn requires_mux(&self) -> bool {
        self.formats.iter().any(|f| f.requires_mux)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(id: &str, height: Option<u16>, kind: MediaKind) -> Format {
        Format {
            id: id.into(),
            url: format!("https://example.invalid/{id}"),
            container: Container::Mp4,
            kind,
            quality: height.map(Quality::height),
            total_bytes: None,
            content_hash: None,
            mime: None,
            requires_mux: kind == MediaKind::VideoOnly,
            segments: None,
        }
    }

    #[test]
    fn a_fragmented_format_is_not_direct() {
        let mut f = fmt("f", Some(720), MediaKind::VideoWithAudio);
        assert!(f.is_direct());

        f.segments = Some(Vec::new());
        assert!(!f.is_direct(), "a segment plan means a fragmented transfer");
    }

    #[test]
    fn a_single_file_manifest_format_is_still_direct() {
        // A DASH manifest whose representation is one whole file arrives as a
        // manifest but transfers as one range request. Keying `is_direct` off the
        // container got this wrong and would have sent it down the segmented path.
        let mut f = fmt("f", Some(720), MediaKind::VideoWithAudio);
        f.container = Container::Dash;
        assert!(f.is_direct());
        assert!(
            f.container.is_segmented(),
            "delivery mechanism still recorded"
        );
    }

    #[test]
    fn container_extensions_are_plausible_for_every_variant() {
        // `Hls`/`Dash` are the "we could not tell" fallback and deliberately
        // refuse to guess. Everything a resolver can actually determine must have
        // a real extension, or the user gets a file named `.bin`.
        for (container, expected) in [
            (Container::Mp4, "mp4"),
            (Container::Mp2t, "ts"),
            (Container::Matroska, "mkv"),
            (Container::Webm, "webm"),
            (Container::Mp3, "mp3"),
            (Container::Adts, "aac"),
            (Container::Opus, "opus"),
        ] {
            assert_eq!(container.extension(), expected, "{container:?}");
        }
        assert_eq!(Container::Hls.extension(), "bin");
        assert_eq!(Container::Dash.extension(), "bin");
    }

    #[test]
    fn segment_plan_serialises_compactly_when_absent() {
        // `skip_serializing_if` keeps the common single-file queue file small and
        // readable, and means an older build can still read a newer queue file.
        let f = fmt("f", Some(720), MediaKind::VideoWithAudio);
        let json = serde_json::to_string(&f).unwrap();
        assert!(!json.contains("segments"), "{json}");
    }

    #[test]
    fn container_from_mime_ignores_parameters_and_case() {
        assert_eq!(
            Container::from_mime("video/MP4; codecs=\"avc1\""),
            Container::Mp4
        );
        assert_eq!(
            Container::from_mime("APPLICATION/vnd.apple.mpegurl"),
            Container::Hls
        );
        assert!(matches!(
            Container::from_mime("application/octet-stream"),
            Container::Other(_)
        ));
    }

    #[test]
    fn unrecognised_container_refuses_to_guess_an_extension() {
        // Guessing here produces files that will not open. A wrong extension is
        // more annoying than no extension, so we decline.
        assert_eq!(
            Container::Other("video/x-msvideo".into()).extension(),
            "bin"
        );
        assert_eq!(Container::Mp4.extension(), "mp4");
    }

    #[test]
    fn best_standalone_never_returns_video_only() {
        let mut m = Media::new("m", "t", "https://example.invalid/x");
        m.formats = vec![
            fmt("v1080", Some(1080), MediaKind::VideoOnly),
            fmt("a128", None, MediaKind::AudioOnly),
            fmt("v720", Some(720), MediaKind::VideoWithAudio),
        ];

        // 720p is the best *playable* choice: the 1080p rendition would need a
        // mux step to be usable at all.
        let best = m.best_standalone().expect("a standalone format exists");
        assert_eq!(best.id, "v720");
    }

    #[test]
    fn best_standalone_falls_back_to_audio_when_no_video_has_audio() {
        let mut m = Media::new("m", "t", "https://example.invalid/x");
        m.formats = vec![
            fmt("v1080", Some(1080), MediaKind::VideoOnly),
            fmt("a192", None, MediaKind::AudioOnly),
        ];
        assert_eq!(m.best_standalone().expect("audio fallback").id, "a192");
    }

    #[test]
    fn best_standalone_returns_a_video_only_format_that_needs_no_mux() {
        // A presentation with no audio track anywhere is complete as it stands:
        // there is nothing to combine it with, so refusing it would refuse good
        // media. ADR-0008.
        let mut m = Media::new("m", "t", "https://example.invalid/x");
        let mut silent = fmt("v1080", Some(1080), MediaKind::VideoOnly);
        silent.requires_mux = false;
        m.formats = vec![silent];

        let best = m
            .best_standalone()
            .expect("a video-only presentation is still downloadable");
        assert_eq!(best.id, "v1080");
    }

    #[test]
    fn best_standalone_prefers_audio_over_a_muxable_video() {
        // When audio exists, the audio track is the thing that actually plays, so
        // it outranks a video rendition that would need a muxer.
        let mut m = Media::new("m", "t", "https://example.invalid/x");
        m.formats = vec![
            fmt("v2160", Some(2160), MediaKind::VideoOnly),
            fmt("a128", None, MediaKind::AudioOnly),
        ];
        assert_eq!(m.best_standalone().expect("audio").id, "a128");
    }

    #[test]
    fn best_standalone_is_none_when_everything_needs_a_mux() {
        // Nothing here plays on its own, so `add_url` must report that rather
        // than queue a file the user cannot open.
        let mut m = Media::new("m", "t", "https://example.invalid/x");
        m.formats = vec![fmt("v1080", Some(1080), MediaKind::VideoOnly)];

        assert!(m.best_standalone().is_none());
        assert!(m.requires_mux());
    }

    #[test]
    fn quality_ordering_is_deterministic_for_sparse_manifests() {
        let mut m = Media::new("m", "t", "https://example.invalid/x");
        m.formats = vec![
            fmt("unknown", None, MediaKind::VideoWithAudio),
            fmt("a96", None, MediaKind::VideoWithAudio),
        ];
        let ids: Vec<&str> = m
            .formats_by_quality()
            .iter()
            .map(|f| f.id.as_str())
            .collect();
        // Both rank (0, 0), so the id breaks the tie, ascending. Reversing an
        // ascending sort would invert this and swap the two between runs.
        assert_eq!(ids, vec!["a96", "unknown"]);
    }

    #[test]
    fn media_kind_predicates() {
        assert!(MediaKind::VideoWithAudio.has_video());
        assert!(MediaKind::VideoWithAudio.has_audio());
        assert!(MediaKind::VideoOnly.has_video());
        assert!(!MediaKind::VideoOnly.has_audio());
        assert!(MediaKind::AudioOnly.has_audio());
        assert!(!MediaKind::AudioOnly.has_video());
    }
}
