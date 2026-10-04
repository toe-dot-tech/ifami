//! HTTP range arithmetic.
//!
//! Pure functions, no I/O. This module is the arithmetic behind resumable
//! transfers, and it is where the subtle correctness bugs live, so it is
//! isolated and exhaustively unit-tested.
//!
//! Terminology: HTTP range ends are **inclusive** (`bytes=0-499` is 500 bytes,
//! RFC 9110 §14.1.1). This module stores `start..end` with `end` **exclusive**,
//! which makes lengths and adjacency arithmetic plain. The conversion from one
//! convention to the other happens in exactly two places —
//! [`parse_range_header`] and [`parse_inclusive_span`] — and both are
//! load-bearing.

/// A half-open byte interval `[start, end)`.
///
/// `end` is `None` for an open-ended range, meaning "through the end of the
/// representation".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ByteRange {
    /// Inclusive first byte.
    pub start: u64,
    /// Exclusive last byte, or `None` for open-ended.
    pub end: Option<u64>,
}

impl ByteRange {
    /// A range from `start` to the end of the representation.
    pub fn from_offset(start: u64) -> Self {
        Self { start, end: None }
    }

    /// A closed range `[start, end)`.
    pub fn closed(start: u64, end: u64) -> Self {
        debug_assert!(start <= end, "range start must not exceed end");
        Self {
            start,
            end: Some(end),
        }
    }

    /// Number of bytes covered, if the range is closed.
    pub fn len(&self) -> Option<u64> {
        self.end.map(|e| e.saturating_sub(self.start))
    }

    /// Whether this range covers no bytes.
    pub fn is_empty(&self) -> bool {
        self.len() == Some(0)
    }

    /// Whether `offset` falls inside this range.
    pub fn contains(&self, offset: u64) -> bool {
        offset >= self.start && self.end.is_none_or(|e| offset < e)
    }

    /// The value for a `Range:` request header.
    ///
    /// ```
    /// # use ifami_core::net::range::ByteRange;
    /// assert_eq!(ByteRange::from_offset(1024).to_header_value(), "bytes=1024-");
    /// assert_eq!(ByteRange::closed(0, 500).to_header_value(), "bytes=0-499");
    /// ```
    pub fn to_header_value(&self) -> String {
        match self.end {
            // Internally `end` is exclusive; on the wire a range end is
            // inclusive (RFC 9110 14.1.1). The two representations differ by
            // one, which is exactly the off-by-one that silently requests one
            // extra byte — or drops the last byte of a segment.
            //
            // `end == start` describes zero bytes, which has no wire
            // representation — an inclusive end equal to `start` means one
            // byte. Constructing an empty range is a caller bug; degrade to
            // open-ended rather than inventing a request nobody asked for.
            Some(end) if end > self.start => format!("bytes={}-{}", self.start, end - 1),
            Some(_) | None => format!("bytes={}-", self.start),
        }
    }

    /// Whether this range can satisfy a continuation request that has `offset`
    /// bytes already stored.
    ///
    /// Used to decide whether a `.part` file can be resumed rather than
    /// restarted. A suffix range (`bytes=-500`) does **not** describe an
    /// absolute offset and can never satisfy one, so it never resumes.
    pub fn can_resume_from(&self, offset: u64) -> bool {
        self.start == 0 && self.end != Some(0) && offset > 0
    }
}

/// A parsed `Content-Range` response header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    /// The byte range actually returned.
    pub range: ByteRange,
    /// Total length of the representation, or `None` for `*/`-terminated.
    pub total: Option<u64>,
}

/// Failure parsing a `Range` request header.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RangeParseError {
    /// The unit was not `bytes`. Multi-range and non-byte units are not used by
    /// Ifami and are rejected rather than guessed at.
    #[error("unsupported range unit in {raw:?}")]
    UnsupportedUnit {
        /// The header as received.
        raw: String,
    },

    /// The syntax was not valid.
    #[error("malformed range header {raw:?}")]
    Malformed {
        /// The header as received.
        raw: String,
    },

    /// The range is structurally valid but unsatisfiable (e.g. `start > end`).
    #[error("unsatisfiable range {start}-{end}")]
    Unsatisfiable {
        /// Start byte.
        start: u64,
        /// Exclusive end byte.
        end: u64,
    },
}

/// Parse a `Range` request header value.
///
/// Supports the two forms Ifami emits: `bytes=<start>-` and
/// `bytes=<start>-<end>`.
///
/// A suffix range (`bytes=-500`) is rejected as malformed. It is a valid HTTP
/// request, but it describes a *length* rather than a position, so it cannot
/// express the "continue from byte N" that resume needs, and accepting it would
/// invite a caller to think it could.
pub fn parse_range_header(value: &str) -> Result<ByteRange, RangeParseError> {
    let raw = value.trim();
    let spec = raw
        .strip_prefix("bytes=")
        .ok_or_else(|| RangeParseError::UnsupportedUnit {
            raw: raw.to_string(),
        })?;

    // Reject multi-range ("bytes=0-99,200-299"). We never request one, and
    // silently taking the first would hide a server disagreement.
    if spec.contains(',') {
        return Err(RangeParseError::UnsupportedUnit {
            raw: raw.to_string(),
        });
    }

    let Some((start_str, end_str)) = spec.split_once('-') else {
        return Err(RangeParseError::Malformed {
            raw: raw.to_string(),
        });
    };

    let start: u64 = start_str
        .trim()
        .parse()
        .map_err(|_| RangeParseError::Malformed {
            raw: raw.to_string(),
        })?;

    let end_str = end_str.trim();
    if end_str.is_empty() {
        return Ok(ByteRange::from_offset(start));
    }

    // HTTP range ends are **inclusive** (RFC 9110 §14.1.1): `bytes=0-499` is 500
    // bytes, and `bytes=100-100` is a perfectly good one-byte request.
    // [`ByteRange`] stores an *exclusive* end, so the `+ 1` happens here and in
    // [`parse_inclusive_span`], and nowhere else. Getting this wrong costs one
    // byte per range and produces a file that is silently one byte short.
    let end: u64 = end_str.parse().map_err(|_| RangeParseError::Malformed {
        raw: raw.to_string(),
    })?;

    if end < start {
        return Err(RangeParseError::Unsatisfiable { start, end });
    }
    let exclusive = end
        .checked_add(1)
        .ok_or(RangeParseError::Unsatisfiable { start, end })?;
    Ok(ByteRange::closed(start, exclusive))
}

/// Parse a `Content-Range` response header value.
///
/// Accepts `bytes <start>-<end>/<total>` and the wildcard `bytes */<total>`,
/// which a `416` response uses to advertise the real length.
///
/// Note the relationship with [`parse_range_header`]: both a request `Range`
/// and a response `Content-Range` use an **inclusive** end, and both are
/// converted to this module's exclusive-end [`ByteRange`] at the point of
/// parsing. A one-byte range (`bytes 0-0/4096`, or `bytes=0-0`) is legal and
/// must parse — it is exactly what a capability probe asks for.
pub fn parse_content_range(value: &str) -> Result<ContentRange, RangeParseError> {
    let raw = value.trim();
    let spec = raw
        .strip_prefix("bytes ")
        .ok_or_else(|| RangeParseError::UnsupportedUnit {
            raw: raw.to_string(),
        })?;

    let (span, total_str) = spec
        .split_once('/')
        .ok_or_else(|| RangeParseError::Malformed {
            raw: raw.to_string(),
        })?;

    let total = if total_str.trim() == "*" {
        None
    } else {
        Some(
            total_str
                .trim()
                .parse::<u64>()
                .map_err(|_| RangeParseError::Malformed {
                    raw: raw.to_string(),
                })?,
        )
    };

    if span.trim() == "*" {
        return Ok(ContentRange {
            range: ByteRange::from_offset(0),
            total,
        });
    }

    let range = parse_inclusive_span(span.trim(), raw)?;
    Ok(ContentRange { range, total })
}

/// Parse an **inclusive** `start-end` span into a half-open [`ByteRange`].
///
/// Used both for `Content-Range` and for DASH `mediaRange`, which share the
/// inclusive-end convention. A span naming a single byte (`0-0`) is legal and
/// yields a one-byte range; `start > end` is not.
pub fn parse_inclusive_span(span: &str, raw: &str) -> Result<ByteRange, RangeParseError> {
    let (start_str, end_str) =
        span.trim()
            .split_once('-')
            .ok_or_else(|| RangeParseError::Malformed {
                raw: raw.to_string(),
            })?;

    let start: u64 = start_str
        .trim()
        .parse()
        .map_err(|_| RangeParseError::Malformed {
            raw: raw.to_string(),
        })?;
    let end: u64 = end_str
        .trim()
        .parse()
        .map_err(|_| RangeParseError::Malformed {
            raw: raw.to_string(),
        })?;

    if end < start {
        return Err(RangeParseError::Unsatisfiable { start, end });
    }
    Ok(ByteRange::closed(start, end + 1))
}

/// Parse a DASH `mediaRange` attribute value.
///
/// DASH spells this `start-end` with an **inclusive** end and no `bytes=`
/// prefix, which is neither an HTTP request range nor a `Content-Range`. Using
/// the request parser here yields an off-by-one on every segment, so every
/// fragmented download would come out one byte short and unplayable.
pub fn parse_media_range(value: &str) -> Result<ByteRange, RangeParseError> {
    let raw = value.trim();
    let spec = raw.strip_prefix("bytes=").unwrap_or(raw);
    parse_inclusive_span(spec, spec)
}

/// Decide how to resume a download.
///
/// This is the function that prevents the most common corruption bug in
/// download managers: a server that ignores `Range` and replies `200 OK` with
/// the *entire* body. A naive client appends that to the existing partial file
/// and produces a file that is double-length and unplayable, usually with no
/// error at all.
///
/// The two cases:
///
/// * Server sent `206 Partial Content` and a `Content-Range` whose start matches
///   our offset, and whose total (when given) matches what we expected:
///   [`ResumeDecision::Append`].
/// * Server sent `200 OK`: it ignored the range. Discard the partial file and
///   restart: [`ResumeDecision::DiscardAndRestart`].
/// * The totals disagree: the object changed under us. Discard and restart:
///   [`ResumeDecision::DiscardAndRestart`].
///
/// [`ResumeDecision::Restart`] is returned when there is nothing to resume from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeDecision {
    /// Start from scratch; there is no partial data.
    Restart,
    /// Append the response body at `offset`.
    Append {
        /// Bytes already on disk.
        offset: u64,
        /// Total length, if known.
        total: Option<u64>,
    },
    /// Throw away the partial data and fetch the whole object.
    DiscardAndRestart,
}

/// Decide how to proceed given what we already have and what the server said.
///
/// * `local_len` — bytes already on disk in the `.part` file.
/// * `expected_total` — the length we believed the object to be, from a prior
///   response, a manifest, or `HEAD`.
/// * `status` — the HTTP status of this response.
/// * `content_range` — the `Content-Range` header, if the server sent one.
pub fn decide_resume(
    local_len: u64,
    expected_total: Option<u64>,
    status: u16,
    content_range: Option<&str>,
) -> ResumeDecision {
    if local_len == 0 {
        return ResumeDecision::Restart;
    }

    // 200 means the server ignored our Range. Never append to the partial file.
    if status != 206 {
        return ResumeDecision::DiscardAndRestart;
    }

    let Some(header) = content_range else {
        // 206 without a Content-Range is malformed, and we cannot prove the
        // offset aligns. Restarting is safe; appending is not.
        return ResumeDecision::DiscardAndRestart;
    };

    let Ok(parsed) = parse_content_range(header) else {
        return ResumeDecision::DiscardAndRestart;
    };

    if parsed.range.start != local_len {
        // The server is resuming from somewhere else entirely.
        return ResumeDecision::DiscardAndRestart;
    }

    match (expected_total, parsed.total) {
        (Some(a), Some(b)) if a != b => {
            // The object changed length between requests. Resuming would splice
            // two different versions together.
            ResumeDecision::DiscardAndRestart
        }
        (Some(expected), _) => ResumeDecision::Append {
            offset: local_len,
            total: Some(expected),
        },
        (None, Some(total)) => ResumeDecision::Append {
            offset: local_len,
            total: Some(total),
        },
        (None, None) => ResumeDecision::Append {
            offset: local_len,
            total: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_open_ended_ranges() {
        let r = parse_range_header("bytes=1024-").unwrap();
        assert_eq!(r, ByteRange::from_offset(1024));
        assert_eq!(r.len(), None);
        assert_eq!(r.to_header_value(), "bytes=1024-");
    }

    #[test]
    fn round_trips_closed_ranges_with_exclusive_end() {
        let r = parse_range_header("bytes=0-499").unwrap();
        assert_eq!(r, ByteRange::closed(0, 500));
        assert_eq!(r.len(), Some(500));
        assert_eq!(r.to_header_value(), "bytes=0-499");
    }

    #[test]
    fn parses_content_range_with_total() {
        let c = parse_content_range("bytes 100-199/1000").unwrap();
        assert_eq!(c.range, ByteRange::closed(100, 200));
        assert_eq!(c.range.len(), Some(100));
        assert_eq!(c.total, Some(1000));
    }

    #[test]
    fn parses_wildcard_content_range_from_416() {
        let c = parse_content_range("bytes */1000").unwrap();
        assert_eq!(c.total, Some(1000));
    }

    #[test]
    fn parses_content_range_with_unknown_total() {
        let c = parse_content_range("bytes 0-99/*").unwrap();
        assert_eq!(c.total, None);
        assert_eq!(c.range.len(), Some(100));
    }

    #[test]
    fn rejects_multipart_ranges() {
        // A multipart response would need a different body parser entirely.
        // Rejecting is correct: silently taking the first part would misalign
        // the file with no error.
        assert_eq!(
            parse_range_header("bytes=0-99,200-299"),
            Err(RangeParseError::UnsupportedUnit {
                raw: "bytes=0-99,200-299".into()
            })
        );
    }

    #[test]
    fn rejects_wrong_unit() {
        assert!(matches!(
            parse_range_header("items=0-99"),
            Err(RangeParseError::UnsupportedUnit { .. })
        ));
    }

    #[test]
    fn rejects_inverted_ranges() {
        assert!(matches!(
            parse_range_header("bytes=200-100"),
            Err(RangeParseError::Unsatisfiable { .. })
        ));
    }

    #[test]
    fn a_single_byte_range_is_satisfiable() {
        // RFC 9110 §14.1.1: `bytes=100-100` asks for one byte and is a valid
        // request. Rejecting it would make a capability probe unable to ask the
        // one question that matters — "will you give me a range at all?".
        let r = parse_range_header("bytes=100-100").unwrap();
        assert_eq!(r, ByteRange::closed(100, 101));
        assert_eq!(r.len(), Some(1));
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["bytes=", "bytes=abc-def", "bytes=-", "0-100", ""] {
            assert!(parse_range_header(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn contains_respects_exclusive_end() {
        let r = ByteRange::closed(10, 20);
        assert!(!r.contains(9));
        assert!(r.contains(10));
        assert!(r.contains(19));
        assert!(!r.contains(20), "end is exclusive");
        assert!(ByteRange::from_offset(10).contains(u64::MAX));
    }

    // --- the resume decision table -----------------------------------------

    #[test]
    fn a_one_byte_response_range_is_legal() {
        // This is exactly what a capability probe asks for and gets back. The
        // request parser rejects `0-0` because a zero-length *request* range
        // means "give me nothing"; a zero-length response span cannot happen,
        // and `0-0` is one byte.
        let c = parse_content_range("bytes 0-0/4096").unwrap();
        assert_eq!(c.range, ByteRange::closed(0, 1));
        assert_eq!(c.range.len(), Some(1));
        assert_eq!(c.total, Some(4096));
    }

    #[test]
    fn dash_media_ranges_are_inclusive() {
        // `mediaRange="0-999"` is bytes 0 through 999, i.e. 1000 bytes. Reading
        // it as a half-open range yields 999 and every fragmented download comes
        // out one byte short.
        assert_eq!(
            parse_media_range("0-999").unwrap(),
            ByteRange::closed(0, 1000)
        );
        assert_eq!(
            parse_media_range("bytes=100-199").unwrap(),
            ByteRange::closed(100, 200)
        );
        assert_eq!(parse_media_range("5-5").unwrap().len(), Some(1));
        assert!(parse_media_range("200-100").is_err());
        assert!(parse_media_range("nonsense").is_err());
    }

    #[test]
    fn response_ranges_reject_inverted_spans() {
        assert!(matches!(
            parse_content_range("bytes 200-100/1000"),
            Err(RangeParseError::Unsatisfiable { .. })
        ));
    }

    #[test]
    fn nothing_on_disk_means_restart() {
        assert_eq!(
            decide_resume(0, Some(500), 200, None),
            ResumeDecision::Restart
        );
        assert_eq!(
            decide_resume(0, Some(500), 206, Some("bytes 0-499/500")),
            ResumeDecision::Restart
        );
    }

    #[test]
    fn server_ignoring_range_forces_restart_and_never_appends() {
        // This is the corruption bug. 200 means the whole body came back.
        assert_eq!(
            decide_resume(300, Some(1000), 200, None),
            ResumeDecision::DiscardAndRestart
        );
    }

    #[test]
    fn partial_content_with_matching_offset_appends() {
        assert_eq!(
            decide_resume(300, Some(1000), 206, Some("bytes 300-999/1000")),
            ResumeDecision::Append {
                offset: 300,
                total: Some(1000)
            }
        );
    }

    #[test]
    fn partial_content_learns_total_when_we_did_not_know_it() {
        assert_eq!(
            decide_resume(300, None, 206, Some("bytes 300-999/1000")),
            ResumeDecision::Append {
                offset: 300,
                total: Some(1000)
            }
        );
    }

    #[test]
    fn partial_content_without_total_still_appends() {
        assert_eq!(
            decide_resume(300, None, 206, Some("bytes 300-999/*")),
            ResumeDecision::Append {
                offset: 300,
                total: None
            }
        );
    }

    #[test]
    fn changed_remote_length_forces_restart() {
        // Splicing two different versions of an object is never correct.
        assert_eq!(
            decide_resume(300, Some(1000), 206, Some("bytes 300-999/2000")),
            ResumeDecision::DiscardAndRestart
        );
    }

    #[test]
    fn mismatched_start_offset_forces_restart() {
        assert_eq!(
            decide_resume(300, Some(1000), 206, Some("bytes 100-999/1000")),
            ResumeDecision::DiscardAndRestart
        );
    }

    #[test]
    fn malformed_content_range_forces_restart() {
        // We cannot prove alignment, and appending unaligned bytes produces a
        // file that looks fine until playback reaches the splice point.
        assert_eq!(
            decide_resume(300, Some(1000), 206, Some("garbage")),
            ResumeDecision::DiscardAndRestart
        );
        assert_eq!(
            decide_resume(300, Some(1000), 206, None),
            ResumeDecision::DiscardAndRestart
        );
    }

    #[test]
    fn zero_byte_prefix_resume_never_satisfies_resumption() {
        // A one-byte range starting at 0 is "give me almost nothing", not
        // "give me everything from here" — resuming from offset 0 is a no-op.
        let r = parse_range_header("bytes=0-0").unwrap();
        assert!(!r.can_resume_from(0));
    }
}
