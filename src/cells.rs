//! A frame is painted into this cell grid first, with colours kept as RGB
//! so they can be blended, and copied to the terminal buffer at the end.

use ratatui::buffer::Buffer;
use ratatui::style::Modifier;
use unicode_width::UnicodeWidthChar;

use crate::color::{self, Rgb};
use crate::fmt;

/// Second cell of a double-width character.
const CONT: char = '\0';
pub const SEAM_LEFT: u8 = 1;
pub const SEAM_BOTTOM: u8 = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct R4 {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl R4 {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
    pub fn inner(&self) -> R4 {
        R4 { x: self.x + 1, y: self.y + 1, w: (self.w - 2).max(0), h: (self.h - 2).max(0) }
    }
}

/// A run of text with its own colour.
pub struct Seg(pub String, pub Rgb, pub bool);

pub fn seg(text: impl Into<String>, fg: Rgb) -> Seg {
    Seg(text.into(), fg, false)
}
pub fn bold(text: impl Into<String>, fg: Rgb) -> Seg {
    Seg(text.into(), fg, true)
}

#[derive(Default)]
pub struct Cells {
    pub w: i32,
    pub h: i32,
    ch: Vec<char>,
    fg: Vec<Rgb>,
    bg: Vec<Rgb>,
    bold: Vec<bool>,
    seam: Vec<u8>,
    pub clip: Option<R4>,
}

impl Cells {
    pub fn reset(&mut self, w: i32, h: i32) {
        let n = (w.max(0) * h.max(0)) as usize;
        self.w = w.max(0);
        self.h = h.max(0);
        self.clip = None;
        for (v, fill) in [(&mut self.fg, color::FG), (&mut self.bg, color::BG)] {
            v.clear();
            v.resize(n, fill);
        }
        self.ch.clear();
        self.ch.resize(n, ' ');
        self.bold.clear();
        self.bold.resize(n, false);
        self.seam.clear();
        self.seam.resize(n, 0);
    }

    fn idx(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.w || y >= self.h || self.clip.is_some_and(|c| !c.contains(x, y)) {
            return None;
        }
        Some((y * self.w + x) as usize)
    }

    /// Writes text and returns the column after it. `bg: None` keeps
    /// whatever background is already there.
    pub fn put(&mut self, mut x: i32, y: i32, s: &str, fg: Rgb, bg: Option<Rgb>, bold: bool) -> i32 {
        for ch in s.chars() {
            let w = ch.width().unwrap_or(0) as i32;
            if w == 0 {
                continue;
            }
            if let Some(i) = self.idx(x, y) {
                self.ch[i] = ch;
                self.fg[i] = fg;
                self.bold[i] = bold;
                self.seam[i] = 0;
                if let Some(b) = bg {
                    self.bg[i] = b;
                }
                if w == 2 {
                    if let Some(j) = self.idx(x + 1, y) {
                        self.ch[j] = CONT;
                        if let Some(b) = bg {
                            self.bg[j] = b;
                            self.seam[j] = 0;
                        }
                    }
                }
            }
            x += w;
        }
        x
    }

    /// Writes segments left to right, truncating at `max_x`.
    pub fn segs(&mut self, mut x: i32, y: i32, max_x: i32, parts: &[Seg]) -> i32 {
        for Seg(text, fg, bold) in parts {
            if text.is_empty() {
                continue;
            }
            let room = max_x - x;
            if room <= 0 {
                break;
            }
            if fmt::width(text) > room {
                return self.put(x, y, &fmt::fit(text, room), *fg, None, *bold);
            }
            x = self.put(x, y, text, *fg, None, *bold);
        }
        x
    }

    pub fn fill(&mut self, r: R4, bg: Rgb) {
        for y in r.y..r.y + r.h {
            for x in r.x..r.x + r.w {
                self.set(x, y, ' ', color::FG, bg, 0);
            }
        }
    }

    /// Replaces one cell outright.
    pub fn set(&mut self, x: i32, y: i32, ch: char, fg: Rgb, bg: Rgb, seam: u8) {
        if let Some(i) = self.idx(x, y) {
            self.ch[i] = ch;
            self.fg[i] = fg;
            self.bg[i] = bg;
            self.bold[i] = false;
            self.seam[i] = seam;
        }
    }

    pub fn bg_at(&self, x: i32, y: i32) -> Option<Rgb> {
        self.idx(x, y).map(|i| self.bg[i])
    }
    pub fn set_bg(&mut self, x: i32, y: i32, bg: Rgb) {
        if let Some(i) = self.idx(x, y) {
            self.bg[i] = bg;
        }
    }

    /// Fades the text of whole rows from `tint` back to its own colour.
    pub fn tint_rows(&mut self, rows: i32, tint: Rgb, t: f32) {
        let n = ((rows.min(self.h) * self.w).max(0)) as usize;
        for fg in &mut self.fg[..n] {
            *fg = color::mix(tint, *fg, t);
        }
    }

    pub fn frame(&mut self, r: R4, fg: Rgb, heavy: bool) {
        if r.w < 2 || r.h < 2 {
            return;
        }
        let g: [&str; 6] = if heavy { ["━", "┃", "┏", "┓", "┗", "┛"] } else { ["─", "│", "┌", "┐", "└", "┘"] };
        let (x1, y1) = (r.x + r.w - 1, r.y + r.h - 1);
        for x in r.x + 1..x1 {
            self.put(x, r.y, g[0], fg, None, false);
            self.put(x, y1, g[0], fg, None, false);
        }
        for y in r.y + 1..y1 {
            self.put(r.x, y, g[1], fg, None, false);
            self.put(x1, y, g[1], fg, None, false);
        }
        self.put(r.x, r.y, g[2], fg, None, false);
        self.put(x1, r.y, g[3], fg, None, false);
        self.put(r.x, y1, g[4], fg, None, false);
        self.put(x1, y1, g[5], fg, None, false);
    }

    /// Draws a titled pane and returns the area inside its border.
    pub fn pane(&mut self, r: R4, title: &str, focused: bool) -> R4 {
        let edge = if focused { color::SQUASH } else { color::BORDER };
        self.frame(r, edge, false);
        let label = format!(" {title} ");
        if fmt::width(&label) <= r.w - 3 {
            self.put(r.x + 2, r.y, &label, if focused { color::INK } else { color::FG }, Some(edge), true);
        }
        r.inner()
    }

    /// Copies the grid into the terminal buffer. Tile seams become thin
    /// block glyphs in the backdrop colour on cells that carry no text: a
    /// line down the left edge of a tile and one along its bottom row, the
    /// two places a label never sits.
    pub fn flush(&self, buf: &mut Buffer, truecolor: bool) {
        let area = buf.area;
        for y in 0..self.h.min(area.height as i32) {
            for x in 0..self.w.min(area.width as i32) {
                let i = (y * self.w + x) as usize;
                let Some(cell) = buf.cell_mut((area.x + x as u16, area.y + y as u16)) else { continue };
                let (mut ch, mut fg) = (self.ch[i], self.fg[i]);
                if ch == ' ' && self.seam[i] != 0 {
                    ch = if self.seam[i] & SEAM_LEFT != 0 { '▏' } else { '▁' };
                    fg = color::BG;
                } else if ch == CONT {
                    ch = ' ';
                }
                cell.set_char(ch);
                cell.set_fg(color::term(fg, truecolor));
                cell.set_bg(color::term(self.bg[i], truecolor));
                cell.modifier = if self.bold[i] { Modifier::BOLD } else { Modifier::empty() };
            }
        }
    }

    /// Text of the grid, with untitled filled cells shown as `▒`.
    #[cfg(test)]
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for y in 0..self.h {
            let mut line = String::new();
            for x in 0..self.w {
                let i = (y * self.w + x) as usize;
                match self.ch[i] {
                    CONT => {} // covered by the wide character before it
                    ' ' if self.bg[i] != color::BG => line.push('▒'),
                    c => line.push(c),
                }
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }
}
