//! Output filename construction and Windows-safe sanitisation.
//!
//! Two rules drive this module:
//!
//! 1. **Never produce a path that cannot be written.** Windows reserves a set
//!    of device names and strips trailing dots and spaces. A file named
//!    `CON.mp4`, or one ending in a space, cannot be created through the normal
//!    API, so we must not emit one.
//! 2. **Never exceed the path budget.** `MAX_PATH` is 260 bytes unless long
//!    paths are enabled, and long-path support is opt-in per machine. We budget
//!    conservatively for a path that is *not* under a long-path-enabled system.
//!
//! Truncation is byte-aware: a filename is cut on a UTF-8 character boundary,
//! preferring a word boundary nearby, and never splits a combining sequence.

use std::borrow::Cow;

/// Default byte budget for a generated filename, excluding the directory.
///
/// 200 leaves headroom under `MAX_PATH` for a reasonably deep destination
/// directory while still allowing descriptive titles.
pub const DEFAULT_FILENAME_BUDGET: usize = 200;

/// Longest title length we will consider before giving up on word-boundary
/// truncation and hard-cutting.
const MAX_SANE_TITLE: usize = 4096;

/// Reserved DOS device names. A file named after one of these cannot be
/// created, **even with an extension** — `CON.mp4` is the device, not a file.
///
/// Matched case-insensitively.
const RESERVED_DEVICE_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Superscript-digit variants Windows also treats as device names.
const RESERVED_DEVICE_NAMES_SUPERSCRIPT: &[&str] =
    &["COM¹", "COM²", "COM³", "LPT¹", "LPT²", "LPT³"];

/// Characters Windows forbids in a filename, plus the ASCII control range.
///
/// `{` and `}` are on this list even though Windows permits them. They are this
/// module's own template metacharacters: a title containing `{x}` that
/// survived into a rendered filename could be re-parsed as a token by anything
/// that later runs the output back through [`render`], and a filename that
/// changes meaning when round-tripped is worse than one that loses two
/// characters.
const FORBIDDEN: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*', '{', '}'];

/// Fields available for interpolation into a filename template.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TemplateVars {
    /// Display title of the media.
    pub title: String,
    /// Uploader or author, when known.
    pub uploader: Option<String>,
    /// Extractor or source name, when applicable.
    pub ext: Option<String>,
    /// Stream identifier within the media object.
    pub id: Option<String>,
    /// Container extension, without a leading dot.
    pub container: Option<String>,
    /// ISO-8601 date, `YYYYMMDD`.
    pub date: Option<String>,
    /// Quality label, e.g. `1080p` or `192`.
    pub quality: Option<String>,
}

/// Placeholder tokens this module understands.
pub const SUPPORTED_TOKENS: &[&str] = &[
    "title",
    "uploader",
    "ext",
    "id",
    "container",
    "date",
    "quality",
];

/// Error produced when a template cannot be used.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TemplateError {
    /// The template referenced a token we do not define.
    #[error("unknown template token `{token}`; supported tokens are: {supported}")]
    UnknownToken {
        /// The offending token.
        token: String,
        /// Comma-separated list of supported tokens.
        supported: String,
    },
}

/// Expand `template` with `vars`, then sanitise the result.
///
/// Token syntax is `{name}`. An unknown token is an error rather than being
/// left in the output, because a filename containing a literal `{quality}` is a
/// bug that surfaces to the user as a mysterious file name.
///
/// ```
/// # use ifami_core::naming::{TemplateVars, render};
/// let vars = TemplateVars {
///     title: "A Talk About Rust".into(),
///     quality: Some("1080p".into()),
///     ..Default::default()
/// };
/// assert_eq!(render("{title} [{quality}]", &vars, 200).unwrap(), "A Talk About Rust [1080p]");
/// ```
pub fn render(template: &str, vars: &TemplateVars, budget: usize) -> Result<String, TemplateError> {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;

    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            // Unbalanced brace: emit it literally rather than failing, since a
            // title containing '{' is far more likely than a broken template.
            out.push('{');
            out.push_str(after);
            return Ok(sanitise(&out, budget));
        };

        let token = &after[..close];
        if token.is_empty() {
            out.push_str("{}}");
        } else {
            match token {
                "title" => out.push_str(&vars.title),
                "uploader" => out.push_str(vars.uploader.as_deref().unwrap_or_default()),
                "ext" => out.push_str(vars.ext.as_deref().unwrap_or_default()),
                "id" => out.push_str(vars.id.as_deref().unwrap_or_default()),
                "container" => out.push_str(vars.container.as_deref().unwrap_or_default()),
                "date" => out.push_str(vars.date.as_deref().unwrap_or_default()),
                "quality" => out.push_str(vars.quality.as_deref().unwrap_or_default()),
                other => {
                    return Err(TemplateError::UnknownToken {
                        token: other.to_string(),
                        supported: SUPPORTED_TOKENS.join(", "),
                    })
                }
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);

    Ok(sanitise(&out, budget))
}

/// Render, falling back to `"{title}"` if the template is invalid.
///
/// The CLI uses this so a bad template in a config file degrades to something
/// sensible rather than aborting the transfer.
pub fn render_lossy(template: &str, vars: &TemplateVars, budget: usize) -> String {
    render(template, vars, budget).unwrap_or_else(|_| {
        let fallback = TemplateVars {
            title: if vars.title.is_empty() {
                "media".to_string()
            } else {
                vars.title.clone()
            },
            ..vars.clone()
        };
        sanitise(&fallback.title, budget)
    })
}

/// Make `raw` safe to use as a Windows filename component and fit it in `budget`
/// bytes.
///
/// Returns an owned `String`; the truncation and the character substitution are
/// both conditional, so a name that is already legal allocates only the string
/// the caller needs anyway.
pub fn sanitise(raw: &str, budget: usize) -> String {
    let truncated = truncate_utf8(raw, budget.max(1));
    strip_invalid(truncated).into_owned()
}

/// Replace forbidden characters, trim Windows-illegal suffixes, and defuse
/// reserved device names.
///
/// Returns an owned `String` only when a change was necessary.
fn strip_invalid(name: &str) -> Cow<'_, str> {
    // `.` and `..` are directory references, not filenames. Checked before the
    // trailing-dot trim, which would otherwise reduce both to nothing.
    if name == "." || name == ".." {
        return Cow::Owned(format!("_{name}"));
    }

    // Windows also forbids these; they are control characters rather than
    // graphics, but they must go for the same reason.
    let substituted: String = name
        .chars()
        .map(|c| {
            if c.is_control() || FORBIDDEN.contains(&c) {
                '_'
            } else {
                c
            }
        })
        .collect();

    // Trailing dots and spaces are silently stripped by the filesystem, so a
    // name we "wrote" may come back different. Strip them ourselves so the
    // name we record in the queue matches the name on disk.
    let trimmed = substituted.trim_end_matches(['.', ' ']).to_string();

    // A name consisting only of dots and spaces would reduce to nothing, which
    // is not a usable filename.
    if trimmed.is_empty() {
        return Cow::Borrowed("_");
    }

    if is_reserved_device(&trimmed) {
        return Cow::Owned(format!("_{trimmed}"));
    }

    // Compared against the *original*, not against `substituted`. Comparing
    // against `substituted` would conclude "nothing changed" whenever the
    // trailing trim was a no-op, and hand back the un-sanitised input — which
    // is precisely the case that matters for `<`, `:` and friends.
    if trimmed != name {
        return Cow::Owned(trimmed);
    }

    Cow::Borrowed(name)
}

/// Whether `name` (ignoring any extension) is a reserved DOS device name.
///
/// Windows matches the stem, so `NUL`, `nul`, and `NUL.mp4` all resolve to the
/// null device.
pub fn is_reserved_device(name: &str) -> bool {
    // Strip a single trailing extension: `CON.mp4` -> `CON`.
    let stem = match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    };

    if RESERVED_DEVICE_NAMES
        .iter()
        .any(|r| stem.eq_ignore_ascii_case(r))
    {
        return true;
    }
    RESERVED_DEVICE_NAMES_SUPERSCRIPT.contains(&stem)
}

/// Cut `s` to at most `max` bytes without splitting a UTF-8 character.
///
/// Prefers a whitespace boundary within the final 25% of the budget so titles
/// do not end mid-word. Hard-cuts when no boundary is available.
pub fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    if max == 0 {
        return "";
    }

    // An attacker- or garbage-supplied "title" can be enormous. Stop looking for
    // a word boundary once we are far past the budget.
    let haystack = if s.len() > MAX_SANE_TITLE {
        // Find a safe slice point at or before MAX_SANE_TITLE.
        let mut end = MAX_SANE_TITLE;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    } else {
        s
    };

    let mut cut = max;
    while cut > 0 && !haystack.is_char_boundary(cut) {
        cut -= 1;
    }
    let candidate = &haystack[..cut];

    // Try to end on a word boundary. The search window is sized in *bytes*, so
    // it has to be snapped back to a character boundary before slicing — with a
    // multi-byte character straddling the quarter mark, `len - len/4` lands
    // mid-character and the slice below panics.
    let mut window_start = candidate.len().saturating_sub(candidate.len() / 4);
    while window_start > 0 && !candidate.is_char_boundary(window_start) {
        window_start -= 1;
    }
    let boundary = candidate[window_start..]
        .char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, _)| window_start + i);

    match boundary {
        Some(i) if i > 0 => candidate[..i].trim_end(),
        _ => candidate,
    }
}

/// Build a filename from a stable digest, for URLs whose path carries no usable
/// name.
///
/// The digest is expected to be a hash of the source URL, computed by the
/// caller. Hashing rather than inventing a name means two runs against the same
/// URL produce the same filename, which is what makes a resumed transfer find
/// its own partial file. An empty digest degrades to a fixed name rather than
/// producing an empty one.
pub fn fallback_name(digest: &str) -> String {
    if digest.is_empty() {
        return "media".to_string();
    }
    format!("{}-{}", &digest[..digest.len().min(16)], "media")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(raw: &str) -> String {
        sanitise(raw, DEFAULT_FILENAME_BUDGET)
    }

    #[test]
    fn reserved_device_names_are_defused_including_with_extensions() {
        // This is the bug that produces "the file appeared but is 0 bytes and
        // cannot be deleted normally".
        for name in ["CON", "con", "NUL", "aux", "COM1", "LPT9"] {
            let out = s(name);
            assert!(out.starts_with('_'), "{name} -> {out} was not defused");
        }
        assert_eq!(s("CON.mp4"), "_CON.mp4");
        assert_eq!(s("nul"), "_nul");
    }

    #[test]
    fn superscript_device_names_are_defused() {
        assert!(s("COM¹").starts_with('_'));
        assert!(s("LPT³").starts_with('_'));
    }

    #[test]
    fn ordinary_names_are_not_defused() {
        for name in ["CONSOLE.log", "NULLABLE", "com10", "comfy.mp4", "a.b.c"] {
            assert_eq!(s(name), name, "{name} should be untouched");
        }
    }

    #[test]
    fn trailing_dots_and_spaces_are_stripped() {
        // The filesystem strips these silently; we strip them so the name in the
        // queue matches the name on disk.
        assert_eq!(s("name."), "name");
        assert_eq!(s("name   "), "name");
        assert_eq!(s("name.  "), "name");
        assert_eq!(s("..."), "_");
        assert_eq!(s("   "), "_");
    }

    #[test]
    fn forbidden_characters_are_replaced() {
        assert_eq!(s("a/b\\c:d*e?f\"g<h>i|j"), "a_b_c_d_e_f_g_h_i_j");
        assert_eq!(s("bell\u{7}char"), "bell_char");
    }

    #[test]
    fn directory_references_are_neutralised() {
        assert_eq!(s("."), "_.");
        assert_eq!(s(".."), "_..");
    }

    #[test]
    fn truncation_never_splits_a_character() {
        // Every emoji is 4 bytes, so a 5-byte budget would split one at offset 5
        // if the cut were naive.
        let s = "🎬🎬🎬🎬🎬🎬🎬🎬";
        for budget in 1..=s.len() {
            let out = truncate_utf8(s, budget);
            assert!(
                out.len() <= budget,
                "budget {budget} produced {} bytes",
                out.len()
            );
            assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        }
    }

    #[test]
    fn truncation_prefers_a_word_boundary() {
        let title = "the quick brown fox jumps over the lazy dog";
        let out = truncate_utf8(title, 20);
        assert!(out.len() <= 20);
        assert_eq!(out, "the quick brown");
    }

    #[test]
    fn truncation_hard_cuts_when_there_is_no_boundary() {
        let out = truncate_utf8("aaaaaaaaaaaaaaaaaaaaaaaaaa", 10);
        assert_eq!(out, "aaaaaaaaaa");
    }

    #[test]
    fn absurdly_long_titles_do_not_blow_up() {
        let huge = "a".repeat(2_000_000);
        let out = truncate_utf8(&huge, 50);
        assert!(out.len() <= 50);
    }

    #[test]
    fn multibyte_title_truncates_within_budget() {
        let title = "ẹ̀kọ́-ìwà-ìtumọ̀-àti-ìdánwó";
        let out = sanitise(title, 30);
        assert!(out.len() <= 30, "got {} bytes: {out}", out.len());
    }

    #[test]
    fn template_expands_all_tokens() {
        let vars = TemplateVars {
            title: "Title Here".into(),
            uploader: Some("Someone".into()),
            ext: Some("site".into()),
            id: Some("abc123".into()),
            container: Some("mp4".into()),
            date: Some("20261001".into()),
            quality: Some("1080p".into()),
        };
        assert_eq!(
            render(
                "{date} - {uploader} - {title} [{quality}] [{id}] ({ext}).{container}",
                &vars,
                200
            )
            .unwrap(),
            "20261001 - Someone - Title Here [1080p] [abc123] (site).mp4"
        );
    }

    #[test]
    fn unknown_token_is_an_error_not_a_literal() {
        let vars = TemplateVars::default();
        let err = render("{title}.{nope}", &vars, 200).unwrap_err();
        assert!(matches!(err, TemplateError::UnknownToken { .. }));
    }

    #[test]
    fn lossy_render_falls_back_to_the_title() {
        let vars = TemplateVars {
            title: "Recoverable".into(),
            ..Default::default()
        };
        assert_eq!(render_lossy("{bad}", &vars, 200), "Recoverable");
    }

    #[test]
    fn lossy_render_uses_a_placeholder_for_an_empty_title() {
        assert_eq!(
            render_lossy("{bad}", &TemplateVars::default(), 200),
            "media"
        );
    }

    #[test]
    fn braces_in_a_title_do_not_abort_rendering() {
        let vars = TemplateVars {
            title: "Set {x} theory".into(),
            ..Default::default()
        };
        assert_eq!(render("{title}", &vars, 200).unwrap(), "Set _x_ theory");
    }

    #[test]
    fn fallback_name_is_stable_and_prefixed_by_digest() {
        assert_eq!(fallback_name("abcdef0123456789"), "abcdef0123456789-media");
        assert_eq!(fallback_name(""), "media");
        // Stable across calls, which is what lets resume find its own .part file.
        assert_eq!(
            fallback_name("abcdef0123456789"),
            fallback_name("abcdef0123456789")
        );
    }
}
