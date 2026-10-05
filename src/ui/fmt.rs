//! Number and text formatting for table cells and stat readouts.
//!
//! These are called for every visible cell, so they write into a caller-owned
//! buffer instead of allocating. egui caches text layout by string content, so
//! keeping the *rendered* text stable between frames (rather than jittering in
//! the last digit) is what actually keeps the table cheap to draw.

use std::fmt::Write;

/// Byte sizes the way Task Manager shows them in tables: always MB, no decimals
/// below 10 MB. Keeps the column visually still instead of flipping units.
pub fn mem_mb(buf: &mut String, bytes: u64) {
    buf.clear();
    if bytes == 0 {
        buf.push('0');
        return;
    }
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb < 0.05 {
        // Anything non-zero still reads as a real, if tiny, allocation.
        buf.push_str("0.1 MB");
        return;
    }
    // Thousands separators matter here; 1234 MB is hard to read at a glance.
    let whole = mb.round() as u64;
    if mb < 10.0 {
        let _ = write!(buf, "{:.1} MB", mb);
    } else {
        group_u64(buf, whole);
        buf.push_str(" MB");
    }
}

/// Adaptive byte size for headline readouts (KB / MB / GB / TB).
pub fn bytes(buf: &mut String, bytes: u64) {
    buf.clear();
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        let _ = write!(buf, "{} {}", bytes, UNITS[0]);
    } else if v < 10.0 {
        let _ = write!(buf, "{:.2} {}", v, UNITS[u]);
    } else if v < 100.0 {
        let _ = write!(buf, "{:.1} {}", v, UNITS[u]);
    } else {
        let _ = write!(buf, "{:.0} {}", v, UNITS[u]);
    }
}

/// Byte rate for the Disk column: blank when idle so the eye is drawn only to
/// processes actually doing I/O.
pub fn rate_mbs(buf: &mut String, bytes_per_sec: f32) {
    buf.clear();
    let mbs = bytes_per_sec / (1024.0 * 1024.0);
    if mbs < 0.05 {
        buf.push('0');
        return;
    }
    let _ = write!(buf, "{:.1} MB/s", mbs);
}

/// Adaptive bit rate for the network graph readout.
pub fn bits_per_sec(buf: &mut String, bytes_per_sec: f32) {
    buf.clear();
    let bits = bytes_per_sec * 8.0;
    if bits < 1000.0 {
        let _ = write!(buf, "{:.0} bps", bits);
    } else if bits < 1_000_000.0 {
        let _ = write!(buf, "{:.1} Kbps", bits / 1000.0);
    } else if bits < 1_000_000_000.0 {
        let _ = write!(buf, "{:.1} Mbps", bits / 1_000_000.0);
    } else {
        let _ = write!(buf, "{:.2} Gbps", bits / 1_000_000_000.0);
    }
}

/// Percentages in tables: one decimal, and a bare `0` when idle.
pub fn percent(buf: &mut String, pct: f32) {
    buf.clear();
    if pct < 0.05 {
        buf.push('0');
        return;
    }
    let _ = write!(buf, "{:.1}", pct);
}

pub fn percent_sign(buf: &mut String, pct: f32) {
    buf.clear();
    let _ = write!(buf, "{:.0}%", pct);
}

/// The same, for the one caller that wants a value rather than a buffer: a dial
/// draws one number, so the buffer would be ceremony.
pub fn percent_sign_of(pct: f32) -> String {
    let mut s = String::new();
    percent_sign(&mut s, pct);
    s
}

/// Thousands-grouped integer using a narrow no-break space, which reads well in
/// a right-aligned numeric column.
pub fn group_u64(buf: &mut String, mut v: u64) {
    if v == 0 {
        buf.push('0');
        return;
    }
    let mut digits = [0u8; 20];
    let mut n = 0;
    while v > 0 {
        digits[n] = (v % 10) as u8;
        v /= 10;
        n += 1;
    }
    for i in (0..n).rev() {
        buf.push((b'0' + digits[i]) as char);
        if i > 0 && i % 3 == 0 {
            buf.push('\u{202F}');
        }
    }
}

pub fn integer(buf: &mut String, v: u64) {
    buf.clear();
    group_u64(buf, v);
}

/// Cumulative CPU time from 100 ns kernel ticks, as `h:mm:ss`.
pub fn cpu_time(buf: &mut String, ticks_100ns: u64) {
    buf.clear();
    let total = ticks_100ns / 10_000_000;
    let _ = write!(
        buf,
        "{}:{:02}:{:02}",
        total / 3600,
        (total % 3600) / 60,
        total % 60
    );
}

/// `d:hh:mm:ss` uptime, as shown on the CPU card.
pub fn uptime(buf: &mut String, secs: u64) {
    buf.clear();
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    let _ = write!(buf, "{d}:{h:02}:{m:02}:{s:02}");
}

/// Link speed for the network card.
pub fn link_speed(buf: &mut String, bits_per_sec: u64) {
    buf.clear();
    if bits_per_sec == 0 {
        buf.push('—');
        return;
    }
    let gbps = bits_per_sec as f64 / 1e9;
    if gbps >= 1.0 {
        let _ = write!(buf, "{:.0} Gbps", gbps);
    } else {
        let _ = write!(buf, "{:.0} Mbps", bits_per_sec as f64 / 1e6);
    }
}

/// Truncate to a character budget with an ellipsis. Used for tooltips and
/// command lines, which can be pathologically long.
pub fn elide(s: &str, max_chars: usize) -> std::borrow::Cow<'_, str> {
    if s.chars().count() <= max_chars {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    std::borrow::Cow::Owned(out)
}

/// Shorten `s` with a trailing ellipsis until it fits `max_w` pixels.
///
/// [`elide`]'s sibling: the same job measured in pixels rather than in characters,
/// for the places that know how much room they have and not how many characters
/// fit in it — a card's label, a statistics row, a device name in a 62px cell.
///
/// Binary search over the character count, so a long device name costs a handful
/// of measurements rather than one per character. The measurements go through
/// egui's layout cache, so a name that did not change between frames is a hash
/// lookup.
pub fn elide_to_width(ui: &egui::Ui, s: &str, font: &egui::FontId, max_w: f32) -> String {
    let measure = |text: &str| -> f32 {
        ui.painter()
            .layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::WHITE)
            .size()
            .x
    };

    if measure(s) <= max_w {
        return s.to_owned();
    }
    let chars: Vec<char> = s.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        let mut cand: String = chars[..mid].iter().collect();
        cand.push('…');
        if measure(&cand) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut out: String = chars[..lo].iter().collect();
    out.push('…');
    out
}

/// ASCII-case-insensitive substring test, without allocating a lowercase copy.
///
/// What every filter box in this window searches with. The obvious spelling —
/// `haystack.to_ascii_lowercase().contains(needle)` — allocates a `String` per
/// field per row, so filtering a 600-row table over five searchable fields is
/// three thousand allocations *per keystroke*. This reads the bytes where they are.
///
/// Non-ASCII bytes compare exactly, which is what is wanted for a process name: the
/// alternative is Unicode case folding, and a task manager's search box is matching
/// file names and service keys, not prose.
pub fn contains_ci(haystack: &str, needle: &str) -> bool {
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    if n.is_empty() || n.len() > h.len() {
        return n.is_empty();
    }
    h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

#[cfg(test)]
mod tests {
    use super::contains_ci;

    /// The search primitive four filter boxes share, so its edges are worth pinning:
    /// an empty needle matches everything (an empty box filters nothing), and a
    /// needle longer than the haystack cannot match however it is cased.
    #[test]
    fn matches_a_substring_whatever_its_case() {
        assert!(contains_ci("chrome.exe", "CHROME"));
        assert!(contains_ci("CHROME.EXE", "chrome"));
        assert!(contains_ci("MsMpEng.exe", "mpeng"));
        assert!(!contains_ci("chrome.exe", "firefox"));

        assert!(contains_ci("anything", ""));
        assert!(contains_ci("", ""));
        assert!(!contains_ci("", "x"));
        assert!(!contains_ci("ab", "abc"));
    }

    /// Non-ASCII compares exactly rather than being case-folded -- the documented
    /// contract, and what keeps a service key matching byte for byte.
    #[test]
    fn leaves_non_ascii_alone() {
        assert!(contains_ci("Explorateur Windows", "explorateur"));
        assert!(contains_ci("café.exe", "café"));
        assert!(!contains_ci("café.exe", "CAFÉ"));
    }
}
