//! Text helpers: sizes, counts, and width-aware truncation.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Human size in powers of 1024, e.g. `13.8M`, `333.0K`, `172G`.
pub fn size(b: u64) -> String {
    const K: f64 = 1024.0;
    let v = b as f64;
    let (d, u) = if v >= K * K * K * K {
        (K * K * K * K, 'T')
    } else if v >= K * K * K {
        (K * K * K, 'G')
    } else if v >= K * K {
        (K * K, 'M')
    } else if v >= K {
        (K, 'K')
    } else {
        return format!("{b}B");
    };
    let x = v / d;
    if x >= 99.95 {
        format!("{x:.0}{u}")
    } else {
        format!("{x:.1}{u}")
    }
}

/// Integer with thousands separators.
pub fn num(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn pct(share: f64) -> String {
    if share >= 0.995 {
        "100%".into()
    } else if share <= 0.0 {
        "0%".into()
    } else if share < 0.01 {
        "<1%".into()
    } else {
        format!("{}%", (share * 100.0).round() as u32)
    }
}

/// Display width in terminal cells.
pub fn width(s: &str) -> i32 {
    UnicodeWidthStr::width(s) as i32
}

/// Truncates to `w` cells, ending in an ellipsis when something was cut.
pub fn fit(s: &str, w: i32) -> String {
    if width(s) <= w {
        return s.to_string();
    }
    if w <= 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0) as i32;
        if used + cw > w - 1 {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

/// Like [`fit`], but keeps the end of the string. Used for long paths.
pub fn tail(s: &str, w: i32) -> String {
    if width(s) <= w {
        return s.to_string();
    }
    if w <= 0 {
        return String::new();
    }
    let mut rev: Vec<char> = Vec::new();
    let mut used = 0;
    for ch in s.chars().rev() {
        let cw = ch.width().unwrap_or(0) as i32;
        if used + cw > w - 1 {
            break;
        }
        rev.push(ch);
        used += cw;
    }
    std::iter::once('…').chain(rev.into_iter().rev()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0B");
        assert_eq!(size(1023), "1023B");
        assert_eq!(size(1024), "1.0K");
        assert_eq!(size(14_470_348), "13.8M");
        assert_eq!(size(172 * 1024 * 1024 * 1024), "172G");
        assert_eq!(size(3 * 1024 * 1024 * 1024 * 1024), "3.0T");
    }

    #[test]
    fn counts_and_shares() {
        assert_eq!(num(0), "0");
        assert_eq!(num(1660), "1,660");
        assert_eq!(num(1_234_567), "1,234,567");
        assert_eq!(pct(1.0), "100%");
        assert_eq!(pct(0.004), "<1%");
        assert_eq!(pct(0.0), "0%");
        assert_eq!(pct(0.456), "46%");
    }

    #[test]
    fn truncation_respects_cell_width() {
        assert_eq!(fit("conference-2025", 20), "conference-2025");
        assert_eq!(fit("conference-2025", 8), "confere…");
        assert_eq!(width(&fit("日本語のファイル名", 7)), 7);
        assert_eq!(tail("~/Projects/webshop/", 9), "…webshop/");
        assert_eq!(fit("abc", 0), "");
    }
}
