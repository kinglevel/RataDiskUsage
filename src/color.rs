//! Colours are carried as 0xRRGGBB so they can be blended, and converted to a
//! terminal colour only when a frame is flushed.

use ratatui::style::Color;

pub type Rgb = u32;

pub const BG: Rgb = 0x17131f;
pub const LIFT: Rgb = 0x221c2d;
pub const FG: Rgb = 0xe9e4f2;
pub const SOFT: Rgb = 0xc3bcd1;
pub const DIM: Rgb = 0x8f879f;
pub const FAINT: Rgb = 0x51495f;
pub const BORDER: Rgb = 0x3d364b;
pub const TRACK: Rgb = 0x262031;
pub const MUTED: Rgb = 0x2f2839;
pub const INK: Rgb = 0x17131f;
pub const WHITE: Rgb = 0xffffff;
pub const SQUASH: Rgb = 0xf0c35a;
pub const GREEN: Rgb = 0x9ed88c;
pub const RED: Rgb = 0xf2806f;

/// Linear blend from `a` (t = 0) to `b` (t = 1).
pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    if t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    let ch = |s: u32| {
        let x = ((a >> s) & 255) as f32;
        let y = ((b >> s) & 255) as f32;
        (x + (y - x) * t) as u32
    };
    ch(16) << 16 | ch(8) << 8 | ch(0)
}

/// OKLCH to sRGB, clamped to the gamut.
pub fn oklch(l: f32, c: f32, h: f32) -> Rgb {
    let (a, b) = (c * h.to_radians().cos(), c * h.to_radians().sin());
    let lp = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let mp = (l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let sp = (l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    let f = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        let g = if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (g * 255.0).round() as u32
    };
    f(4.076_741_7 * lp - 3.307_711_6 * mp + 0.230_969_94 * sp) << 16
        | f(-1.268_438 * lp + 2.609_757_4 * mp - 0.341_319_38 * sp) << 8
        | f(-0.004_196_086_3 * lp - 0.703_418_6 * mp + 1.707_614_7 * sp)
}

/// Hues for file types. A type gets a slot the first time it is among the
/// largest types of the scan and keeps it, so colours never shuffle while a
/// scan is still running. Slots are stepped by the golden angle so that
/// neighbouring slots land far apart on the colour wheel.
#[derive(Default)]
pub struct Palette {
    slots: Vec<u16>,
    next: u16,
}

const NO_SLOT: u16 = u16::MAX;
const RANKED: usize = 12;

impl Palette {
    pub fn color(&self, ext: u16, l: f32) -> Rgb {
        if ext == crate::tree::NO_EXT {
            return oklch(l, 0.035, 285.0);
        }
        match self.slots.get(ext as usize).copied().unwrap_or(NO_SLOT) {
            NO_SLOT => oklch(l, 0.022, 310.0),
            s => oklch(l, 0.112, (28.0 + s as f32 * 137.508) % 360.0),
        }
    }

    /// Gives a slot to each of the largest types that does not have one yet.
    pub fn assign_top(&mut self, types: &[(u16, u64)]) {
        let mut top: Vec<(u16, u64)> = types.iter().copied().filter(|t| t.0 != crate::tree::NO_EXT && t.1 > 0).collect();
        top.sort_unstable_by_key(|t| std::cmp::Reverse(t.1));
        for &(ext, _) in top.iter().take(RANKED) {
            let i = ext as usize;
            if self.slots.len() <= i {
                self.slots.resize(i + 1, NO_SLOT);
            }
            if self.slots[i] == NO_SLOT {
                self.slots[i] = self.next;
                self.next += 1;
            }
        }
    }
}

/// Converts to a terminal colour, falling back to the 256-colour palette
/// when the terminal does not advertise 24-bit colour.
pub fn term(c: Rgb, truecolor: bool) -> Color {
    let (r, g, b) = ((c >> 16) & 255, (c >> 8) & 255, c & 255);
    if truecolor {
        return Color::Rgb(r as u8, g as u8, b as u8);
    }
    const CUBE: [u32; 6] = [0, 95, 135, 175, 215, 255];
    let q = |v: u32| {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            ((v - 35) / 40).min(5)
        }
    };
    let (qr, qg, qb) = (q(r), q(g), q(b));
    let dist = |x: u32, y: u32, z: u32| {
        let d = |p: u32, s: u32| (p as i32 - s as i32).pow(2);
        d(x, r) + d(y, g) + d(z, b)
    };
    let cube = dist(CUBE[qr as usize], CUBE[qg as usize], CUBE[qb as usize]);
    let avg = (r + g + b) / 3;
    let gi = if avg < 8 { 0 } else { ((avg - 8) / 10).min(23) };
    let gv = 8 + gi * 10;
    if dist(gv, gv, gv) < cube {
        Color::Indexed((232 + gi) as u8)
    } else {
        Color::Indexed((16 + 36 * qr + 6 * qg + qb) as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_endpoints_and_midpoint() {
        assert_eq!(mix(0x000000, 0xffffff, 0.0), 0x000000);
        assert_eq!(mix(0x000000, 0xffffff, 1.0), 0xffffff);
        assert_eq!(mix(0x000000, 0xfefefe, 0.5), 0x7f7f7f);
    }

    #[test]
    fn oklch_greys_are_neutral() {
        assert_eq!(oklch(0.0, 0.0, 0.0), 0x000000);
        assert_eq!(oklch(1.0, 0.0, 0.0), 0xffffff);
        let mid = oklch(0.6, 0.0, 0.0);
        assert_eq!((mid >> 16) & 255, mid & 255);
    }

    #[test]
    fn palette_slots_are_sticky() {
        let mut p = Palette::default();
        p.assign_top(&[(1, 10), (2, 50)]);
        let first = p.color(2, 0.7);
        p.assign_top(&[(1, 500), (2, 50), (3, 900)]);
        assert_eq!(p.color(2, 0.7), first);
        assert_ne!(p.color(3, 0.7), p.color(1, 0.7));
    }

    #[test]
    fn falls_back_to_indexed_colour() {
        assert_eq!(term(0xffffff, false), Color::Indexed(231));
        assert_eq!(term(0x000000, false), Color::Indexed(16));
        assert_eq!(term(0x123456, true), Color::Rgb(0x12, 0x34, 0x56));
    }
}
