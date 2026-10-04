//! Presentation only.
//!
//! Nothing in here decides anything or touches the filesystem. If a rule starts
//! leaking out of these functions — "a task with no total should show `-`",
//! "failed tasks sort first" — it belongs in `ifami-core`, where it can be
//! tested without a terminal.

/// Human-readable byte count, e.g. `4.1 MB`.
///
/// Binary units, because that is what operating systems report, and a file that
/// Windows calls 1.00 GB should not be 0.93 GB here.
///
/// The decimal place is dropped at three significant figures, not two: below
/// 100 the value gets one decimal (`15.0 MiB`) and at or above it none
/// (`999 MiB`). Two digits would give `99.9 MiB` at eight characters and a
/// `SIZE` column that shifts as a download runs. Every figure from KiB upward is
/// therefore at most eight characters wide, and at most seven outside the
/// `10.0`–`99.9` band of each unit.
pub fn bytes(total: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];

    if total < 1024 {
        return format!("{total} B");
    }

    let mut value = total as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// The note printed while transfers run.
///
/// Printed without a newline, because the manager owns the output until it goes
/// idle and this is the only thing standing between the user and silence.
pub fn spinner_note(count: &usize) -> String {
    match *count {
        0 => String::new(),
        1 => "fetching 1 link; press Ctrl-C to stop (a partial file stays resumable)\n".to_string(),
        n => format!("fetching {n} links; press Ctrl-C to stop (partial files stay resumable)\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_counts_use_binary_units() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(1024 * 1024), "1.0 MiB");
        // 1 GiB is 1073741824 bytes, not 1e9: quoting the same figure Windows
        // shows is the whole point.
        assert_eq!(bytes(1024 * 1024 * 1024), "1.0 GiB");
    }

    #[test]
    fn byte_counts_drop_the_decimal_when_it_would_not_add_information() {
        assert_eq!(bytes(150 * 1024 * 1024), "150 MiB");
        assert_eq!(bytes(15 * 1024 * 1024), "15.0 MiB");
    }

    #[test]
    fn byte_counts_do_not_overflow_the_largest_unit() {
        // u64::MAX is 16 EiB, so this must stop at EiB rather than running off
        // the end of the unit table.
        assert_eq!(bytes(u64::MAX), "16.0 EiB");
    }

    #[test]
    fn figures_never_blow_out_the_column() {
        // Bounding the rendered width is what stops the queue listing's SIZE
        // column from resizing itself as a download runs.
        for total in [
            1u64,
            1023,
            1024,
            15 * 1024 * 1024,
            99 * 1024 * 1024,
            100 * 1024 * 1024,
            999 * 1024 * 1024,
            1024 * 1024 * 1024,
            1024u64.pow(4),
            1024u64.pow(5),
            1024u64.pow(6),
            u64::MAX,
        ] {
            let rendered = bytes(total);
            assert!(
                rendered.chars().count() <= 8,
                "{total} rendered as {rendered:?}, which is wider than the column allows"
            );
        }
    }

    #[test]
    fn the_spinner_note_agrees_in_gender_and_number() {
        assert_eq!(spinner_note(&0), "");
        assert!(spinner_note(&1).contains("1 link"));
        assert!(spinner_note(&3).contains("3 links"));
    }
}
