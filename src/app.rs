//! Application state and behaviour: navigation, layout transitions, the
//! live scan and deletion. Painting lives in `ui.rs`.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};

use crate::cells::{Cells, R4};
use crate::color::{self, Palette, Rgb};
use crate::fmt;
use crate::scan::Scanner;
use crate::tree::{Batch, Id, Tree, NONE};
use crate::treemap::{squarify, RectF};

/// One pass of the sheen over the selected tile after the last input.
pub const SHEEN: f64 = 3200.0;
/// Header text fades back from the flash colour over this long.
pub const T_FLASH: f64 = 240.0;
/// How long a flash message stays in the header.
pub const FLASH_SHOWN: f64 = 2600.0;
pub const T_DISSOLVE: f64 = 340.0;
const T_ROUTINE: f64 = 160.0;
const T_NAV: f64 = 260.0;
const T_REVEAL: f64 = 280.0;
/// The map is laid out again at most this often while a scan streams in.
const BATCH_MS: f64 = 140.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Map,
    Files,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Modal {
    Delete(Id),
    Help,
}

pub struct Flash {
    pub t0: f64,
    pub color: Rgb,
    pub msg: String,
}

/// One rectangle of the map: a file, a folder, or the bucket of entries
/// too small to draw (`node == NONE`).
#[derive(Clone, Copy, Debug)]
pub struct Tile {
    pub key: u64,
    pub node: Id,
    pub parent: Id,
    /// Entries folded into a bucket tile.
    pub hidden: u32,
    pub depth: u8,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub v: u64,
    pub share: f32,
    /// A folder tile whose contents are drawn inside it.
    pub nested: bool,
    pub bg: Rgb,
    pub ink: Rgb,
}

impl Tile {
    pub fn small(&self) -> bool {
        self.node == NONE
    }
    pub fn rect(&self) -> RectF {
        RectF { x: self.x as f64, y: self.y as f64, w: self.w as f64, h: self.h as f64 }
    }
    pub fn r4(&self) -> R4 {
        R4 { x: self.x, y: self.y, w: self.w, h: self.h }
    }
}

pub fn small_key(parent: Id) -> u64 {
    1 << 32 | parent as u64
}

/// A tile on screen and where it is animating from.
pub struct Shown {
    pub tile: Tile,
    pub from: RectF,
    pub fade: bool,
}

/// A tile of the previous layout on its way out.
pub struct Leaving {
    pub tile: Tile,
    pub from: RectF,
    pub to: RectF,
}

#[derive(Clone, Copy, Default)]
pub struct Anim {
    pub t0: f64,
    pub dur: f64,
}

pub struct Dissolve {
    pub node: Id,
    pub t0: f64,
    /// Whether the tile is on the map and motion is on.
    pub visual: bool,
    rx: Receiver<io::Result<()>>,
    result: Option<io::Result<()>>,
}

#[derive(Clone, Copy)]
pub enum Kind {
    Instant,
    Routine,
    Nav,
    Reveal,
}

#[derive(Clone, Copy)]
pub enum Hint {
    None,
    /// Drilling into this folder.
    In(Id),
    /// Coming back out of this folder.
    Out(Id),
}

#[derive(Clone, Copy, Default)]
pub struct Regions {
    pub narrow: bool,
    pub too_small: bool,
    pub files: R4,
    pub types: R4,
    pub map: R4,
    pub insp: R4,
    pub strip_y: i32,
    pub map_in: R4,
}

/// Splits the screen. Two header rows on top, key hints on the last row.
pub fn regions(cols: i32, rows: i32) -> Regions {
    let top = 2;
    let h = rows - 1 - top;
    if cols < 30 || rows < 12 {
        return Regions { too_small: true, ..Regions::default() };
    }
    if cols < 64 {
        let insp_h = if h >= 18 { 5 } else { 4 };
        let main = R4 { x: 0, y: top, w: cols, h: h - insp_h - 1 };
        return Regions {
            narrow: true,
            files: main,
            map: main,
            insp: R4 { x: 0, y: top + main.h + 1, w: cols, h: insp_h },
            strip_y: top + main.h,
            map_in: main.inner(),
            ..Regions::default()
        };
    }
    let left = if cols >= 96 { ((cols as f64 * 0.36).round() as i32).clamp(38, 54) } else { 30 };
    let types_h = (if cols >= 96 { 10 } else { 8 }).min((h / 3).max(4));
    let insp_h = if h >= 18 { 6 } else { 5 };
    let map = R4 { x: left + 1, y: top, w: cols - left - 1, h: h - insp_h };
    Regions {
        files: R4 { x: 0, y: top, w: left, h: h - types_h },
        types: R4 { x: 0, y: top + h - types_h, w: left, h: types_h },
        map,
        insp: R4 { x: left + 1, y: top + h - insp_h, w: cols - left - 1, h: insp_h },
        map_in: map.inner(),
        ..Regions::default()
    }
}

pub struct Config {
    /// No tweens, sheen, dissolve or header fades.
    pub reduced: bool,
    /// Deleting only removes entries from the view.
    pub dry_run: bool,
    pub truecolor: bool,
    /// Height of a terminal cell divided by its width.
    pub aspect: f64,
}

pub struct App {
    pub tree: Tree,
    scanner: Option<Scanner>,
    rx: Option<Receiver<Batch>>,
    last_layout: f64,
    pub cwd: Id,
    pub sel: Id,
    /// The selection follows the largest entry until the user moves it.
    sel_auto: bool,
    pub focus: Focus,
    pub expanded: HashSet<Id>,
    pub scroll: usize,
    pub modal: Option<Modal>,
    pub freed: u64,
    pub flash: Option<Flash>,
    /// Milliseconds since start, set by the event loop before each step.
    pub now: f64,
    pub last_input: f64,
    pub disp: HashMap<u64, Shown>,
    pub leaving: Vec<Leaving>,
    pub anim: Anim,
    pub dissolve: Option<Dissolve>,
    pub palette: Palette,
    pub rows: Vec<(Id, u16)>,
    rows_dirty: bool,
    pub legend: Vec<(u16, u64)>,
    pub legend_total: u64,
    legend_dirty: bool,
    pub regions: Regions,
    cols: i32,
    lines: i32,
    pub cfg: Config,
    pub quit: bool,
    pub cells: Cells,
    pub hit_rows: Vec<(i32, Id)>,
    pub files_in: R4,
}

pub fn ease(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

fn remove_path(path: &Path) -> io::Result<()> {
    // symlink_metadata: a link to a folder is removed as a link, never followed.
    if std::fs::symlink_metadata(path)?.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

impl App {
    pub fn new(tree: Tree, scanner: Option<Scanner>, cfg: Config) -> Self {
        App {
            cwd: tree.root,
            tree,
            scanner,
            rx: None,
            last_layout: 0.0,
            sel: NONE,
            sel_auto: true,
            focus: Focus::Map,
            expanded: HashSet::new(),
            scroll: 0,
            modal: None,
            freed: 0,
            flash: None,
            now: 0.0,
            last_input: 0.0,
            disp: HashMap::new(),
            leaving: Vec::new(),
            anim: Anim::default(),
            dissolve: None,
            palette: Palette::default(),
            rows: Vec::new(),
            rows_dirty: true,
            legend: Vec::new(),
            legend_total: 0,
            legend_dirty: true,
            regions: Regions::default(),
            cols: 0,
            lines: 0,
            cfg,
            quit: false,
            cells: Cells::default(),
            hit_rows: Vec::new(),
            files_in: R4::default(),
        }
    }

    pub fn scanning(&self) -> bool {
        self.rx.is_some()
    }

    fn say(&mut self, color: Rgb, msg: impl Into<String>) {
        self.flash = Some(Flash { t0: self.now, color, msg: msg.into() });
    }

    fn invalidate(&mut self) {
        self.rows_dirty = true;
        self.legend_dirty = true;
    }

    /// Scans the current folder from scratch.
    pub fn start_scan(&mut self) {
        let Some(scanner) = &self.scanner else { return };
        let Some(path) = self.tree.path(self.cwd) else { return };
        scanner.forget(&self.tree.descendants(self.cwd));
        self.tree.clear_children(self.cwd);
        self.rx = Some(scanner.start(self.cwd, path));
        self.sel = NONE;
        self.sel_auto = true;
        self.expanded.clear();
        self.scroll = 0;
        self.disp.clear();
        self.leaving.clear();
        self.anim = Anim::default();
        self.last_layout = self.now;
        self.invalidate();
    }

    /// Takes in scanner output and finished deletions. Returns whether
    /// anything on screen may have changed, and whether more scanner
    /// output is already waiting.
    pub fn pump(&mut self) -> (bool, bool) {
        let (mut changed, mut backlog) = (false, false);
        if let Some(rx) = &self.rx {
            let started = Instant::now();
            let (mut got, mut done) = (false, false);
            loop {
                match rx.try_recv() {
                    Ok(batch) => {
                        self.tree.apply(batch);
                        got = true;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        done = true;
                        break;
                    }
                }
                if started.elapsed() > Duration::from_millis(12) {
                    backlog = true;
                    break;
                }
            }
            changed |= got || done;
            if done {
                self.rx = None;
                self.relayout(Kind::Routine, Hint::None);
                if self.tree.errors > 0 {
                    self.say(color::SQUASH, format!("Scan complete, {} unreadable", fmt::num(self.tree.errors)));
                } else {
                    self.say(color::GREEN, "Scan complete");
                }
            } else if got && self.now - self.last_layout >= BATCH_MS {
                let kind = if self.disp.is_empty() { Kind::Reveal } else { Kind::Routine };
                self.relayout(kind, Hint::None);
            }
        }
        let mut finished = false;
        if let Some(d) = &mut self.dissolve {
            if d.result.is_none() {
                match d.rx.try_recv() {
                    Ok(r) => d.result = Some(r),
                    Err(TryRecvError::Disconnected) => d.result = Some(Err(io::Error::other("the delete worker stopped"))),
                    Err(TryRecvError::Empty) => {}
                }
            }
            finished = d.result.is_some() && (!d.visual || self.now - d.t0 >= T_DISSOLVE);
        }
        if finished {
            let d = self.dissolve.take().unwrap();
            self.finish_delete(d);
            changed = true;
        }
        (changed, backlog)
    }

    /// Whether frames should keep coming without waiting for input.
    pub fn busy(&self) -> bool {
        self.scanning()
            || self.dissolve.is_some()
            || (self.anim.dur > 0.0 && self.now - self.anim.t0 < self.anim.dur + 40.0)
            || (!self.cfg.reduced && self.now - self.last_input < SHEEN + 80.0)
            || self.flash.as_ref().is_some_and(|f| self.now - f.t0 < FLASH_SHOWN + 100.0)
    }

    pub fn anim_t(&self) -> f64 {
        if self.anim.dur > 0.0 {
            ((self.now - self.anim.t0) / self.anim.dur).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    pub fn resize(&mut self, cols: i32, lines: i32) {
        if cols == self.cols && lines == self.lines {
            return;
        }
        self.cols = cols;
        self.lines = lines;
        self.regions = regions(cols, lines);
        self.relayout(if self.disp.is_empty() { Kind::Instant } else { Kind::Routine }, Hint::None);
    }

    // ---------- layout ----------

    fn tile_style(&self, t: &Tile) -> (Rgb, Rgb) {
        if t.small() {
            return (color::MUTED, color::DIM);
        }
        let ext = self.tree.dominant(t.node);
        if t.nested {
            return (self.palette.color(ext, 0.38), color::FG);
        }
        // A bigger share of the parent is brighter.
        let l = (if t.depth == 1 { 0.58 } else { 0.52 } + 0.2 * t.share.sqrt()).min(0.8);
        (self.palette.color(ext, l), color::INK)
    }

    /// Lays the children of `dir` out in the given cell rectangle. Entries
    /// too small to draw are folded into one bucket tile. Folders at the
    /// first level get their own children drawn inside them.
    fn place(&mut self, dir: Id, area: R4, depth: u8, out: &mut Vec<Tile>) {
        let R4 { x, y, w, h } = area;
        if w < 1 || h < 1 {
            return;
        }
        self.tree.sort_kids(dir);
        let kids = self.tree.kids(dir);
        let n_kids = kids.len();
        let total: u64 = kids.iter().map(|&k| self.tree.size(k)).sum();
        if total == 0 {
            return;
        }
        let cells = (w * h) as f64;
        let (min_area, min_w) = if depth == 1 { (12.0, 5) } else { (6.0, 3) };
        let mut big: Vec<(Id, u64)> = Vec::new();
        for &k in kids {
            let v = self.tree.size(k);
            if v == 0 || big.len() >= 48 || v as f64 / total as f64 * cells < min_area {
                break;
            }
            big.push((k, v));
        }
        // Cells are taller than wide, so squarify in a space where a cell is
        // square and scale back, otherwise tiles come out as tall slivers.
        let a = self.cfg.aspect;
        let placed: Vec<(Id, u64, R4)> = loop {
            let small_sum = total - big.iter().map(|b| b.1).sum::<u64>();
            let mut items = big.clone();
            if n_kids > big.len() && small_sum > 0 {
                let at = items.partition_point(|it| it.1 >= small_sum);
                items.insert(at, (NONE, small_sum));
            }
            let vals: Vec<f64> = items.iter().map(|it| it.1 as f64).collect();
            let rects = squarify(&vals, x as f64, y as f64 * a, w as f64, h as f64 * a);
            let snapped: Vec<(Id, u64, R4)> = items
                .iter()
                .zip(rects)
                .map(|(&(id, v), r)| {
                    let (x0, x1) = (r.x.round() as i32, (r.x + r.w).round() as i32);
                    let (y0, y1) = ((r.y / a).round() as i32, ((r.y + r.h) / a).round() as i32);
                    (id, v, R4 { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
                })
                .collect();
            if big.len() > 1 && snapped.iter().any(|s| s.0 != NONE && (s.2.w < min_w || s.2.h < 1)) {
                big.pop();
                continue;
            }
            break snapped;
        };
        let hidden = (n_kids - big.len()) as u32;
        for (id, v, r) in placed {
            if r.w < 1 || r.h < 1 {
                continue;
            }
            let at = out.len();
            out.push(Tile {
                key: if id == NONE { small_key(dir) } else { id as u64 },
                node: id,
                parent: dir,
                hidden: if id == NONE { hidden } else { 0 },
                depth,
                x: r.x,
                y: r.y,
                w: r.w,
                h: r.h,
                v,
                share: (v as f64 / total as f64) as f32,
                nested: false,
                bg: 0,
                ink: 0,
            });
            if depth == 1 && id != NONE && self.tree.is_dir(id) && r.w >= 10 && r.h >= 4 {
                self.place(id, R4 { x: r.x, y: r.y + 1, w: r.w, h: r.h - 1 }, 2, out);
                out[at].nested = out.len() > at + 1;
            }
            let (bg, ink) = self.tile_style(&out[at]);
            out[at].bg = bg;
            out[at].ink = ink;
        }
    }

    /// Computes the map for the current folder and starts the transition
    /// from whatever is on screen now.
    pub fn relayout(&mut self, kind: Kind, hint: Hint) {
        self.invalidate();
        self.last_layout = self.now;
        self.palette.assign_top(self.tree.types(self.tree.root));
        let m = self.regions.map_in;
        let mut tiles = Vec::new();
        if !self.regions.too_small {
            self.place(self.cwd, m, 1, &mut tiles);
        }
        if self.sel == NONE || self.sel_auto {
            self.tree.sort_kids(self.cwd);
            self.sel = self.tree.kids(self.cwd).first().copied().unwrap_or(NONE);
        }
        let e0 = ease(self.anim_t());
        let old = std::mem::take(&mut self.disp);
        let mf = RectF { x: m.x as f64, y: m.y as f64, w: m.w as f64, h: m.h as f64 };
        let dur = match kind {
            _ if self.cfg.reduced || mf.w < 1.0 || mf.h < 1.0 => 0.0,
            Kind::Instant => 0.0,
            Kind::Routine => T_ROUTINE,
            Kind::Nav => T_NAV,
            Kind::Reveal => T_REVEAL,
        };
        // `zoom` = (src, dst): a spot in the new layout and where that spot
        // was on screen before, so the map appears to zoom in or out.
        let zoom: Option<(RectF, RectF)> = match hint {
            _ if dur == 0.0 => None,
            Hint::In(n) => {
                old.get(&(n as u64)).map(|o| o.from.lerp(o.tile.rect(), e0)).filter(|r| r.w >= 1.0 && r.h >= 1.0).map(|r0| (mf, r0))
            }
            Hint::Out(prev) => tiles.iter().find(|t| t.key == prev as u64).map(|t| (t.rect(), mf)),
            Hint::None => None,
        };
        let mut next = HashMap::with_capacity(tiles.len());
        for t in tiles {
            let (from, fade) = if dur == 0.0 {
                (t.rect(), false)
            } else if let Some(o) = old.get(&t.key) {
                (o.from.lerp(o.tile.rect(), e0), false)
            } else if let Some((src, dst)) = zoom {
                (t.rect().map(src, dst), true)
            } else {
                (RectF { x: t.x as f64 + t.w as f64 / 2.0, y: t.y as f64 + t.h as f64 / 2.0, w: 0.0, h: 0.0 }, true)
            };
            next.insert(t.key, Shown { tile: t, from, fade });
        }
        self.leaving.clear();
        if dur > 0.0 {
            for (k, o) in &old {
                if !next.contains_key(k) {
                    let cur = o.from.lerp(o.tile.rect(), e0);
                    let to = zoom.map_or(cur, |(src, dst)| cur.map(dst, src));
                    self.leaving.push(Leaving { tile: o.tile, from: cur, to });
                }
            }
        }
        self.disp = next;
        self.anim = Anim { t0: self.now, dur };
    }

    // ---------- selection ----------

    /// Key of the tile that represents the selection on the map: its own
    /// tile, the bucket it was folded into, or the nearest drawn ancestor.
    pub fn sel_key(&self) -> Option<u64> {
        let mut a = self.sel;
        while a != NONE && a != self.cwd {
            if self.disp.contains_key(&(a as u64)) {
                return Some(a as u64);
            }
            let p = self.tree.parent(a);
            if p == NONE {
                return None;
            }
            if self.disp.contains_key(&small_key(p)) {
                return Some(small_key(p));
            }
            a = p;
        }
        None
    }

    /// The first-level tile the selection sits in.
    fn top_tile(&self) -> Option<Tile> {
        let mut a = self.sel;
        while a != NONE && self.tree.parent(a) != self.cwd {
            a = self.tree.parent(a);
        }
        if a == NONE {
            return None;
        }
        self.disp.get(&(a as u64)).or_else(|| self.disp.get(&small_key(self.cwd))).map(|s| s.tile)
    }

    fn push_rows(&mut self, dir: Id, depth: u16, out: &mut Vec<(Id, u16)>) {
        self.tree.sort_kids(dir);
        for i in 0..self.tree.kids(dir).len() {
            let k = self.tree.kids(dir)[i];
            out.push((k, depth));
            if self.expanded.contains(&k) && self.tree.is_dir(k) {
                self.push_rows(k, depth + 1, out);
            }
        }
    }

    /// Rows of the file list: the current folder's children, largest
    /// first, with expanded folders opened in place.
    pub fn ensure_rows(&mut self) {
        if !self.rows_dirty {
            return;
        }
        self.rows_dirty = false;
        let mut rows = std::mem::take(&mut self.rows);
        rows.clear();
        self.push_rows(self.cwd, 0, &mut rows);
        self.rows = rows;
    }

    /// Types below the current folder, largest first.
    pub fn ensure_legend(&mut self) {
        if !self.legend_dirty {
            return;
        }
        self.legend_dirty = false;
        let mut v: Vec<(u16, u64)> = self.tree.types(self.cwd).iter().copied().filter(|t| t.1 > 0).collect();
        v.sort_by_key(|t| std::cmp::Reverse(t.1));
        self.legend_total = v.iter().map(|t| t.1).sum();
        self.legend = v;
    }

    // ---------- actions ----------

    /// Moves to the neighbouring tile in a direction: the nearest one that
    /// overlaps the current tile's span, preferring the largest overlap.
    fn move_map(&mut self, dx: i32, dy: i32) {
        let tiles: Vec<Tile> = self.disp.values().map(|s| s.tile).filter(|t| t.depth == 1 && !t.small()).collect();
        let Some(o) = self.top_tile() else {
            if let Some(t) = tiles.iter().max_by_key(|t| t.v) {
                self.sel = t.node;
            }
            return;
        };
        let mut best: Option<(i32, i32, Id)> = None;
        for t in &tiles {
            if t.key == o.key {
                continue;
            }
            let (d, ov) = if dx != 0 {
                (if dx > 0 { t.x - (o.x + o.w) } else { o.x - (t.x + t.w) }, (o.y + o.h).min(t.y + t.h) - o.y.max(t.y))
            } else {
                (if dy > 0 { t.y - (o.y + o.h) } else { o.y - (t.y + t.h) }, (o.x + o.w).min(t.x + t.w) - o.x.max(t.x))
            };
            if d < 0 || ov <= 0 {
                continue;
            }
            if best.is_none_or(|(bd, bo, _)| d < bd || (d == bd && ov > bo)) {
                best = Some((d, ov, t.node));
            }
        }
        if let Some((_, _, n)) = best {
            self.sel = n;
        }
    }

    fn move_row(&mut self, d: i64) {
        self.ensure_rows();
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as i64 - 1;
        let i = match self.rows.iter().position(|r| r.0 == self.sel) {
            Some(i) => (i as i64 + d).clamp(0, last),
            None => 0,
        };
        self.sel = self.rows[i as usize].0;
    }

    fn expand(&mut self) {
        let n = self.sel;
        if n == NONE || !self.tree.is_dir(n) || self.tree.kids(n).is_empty() {
            return;
        }
        if self.expanded.insert(n) {
            self.rows_dirty = true;
        } else {
            self.tree.sort_kids(n);
            self.sel = self.tree.kids(n)[0];
        }
    }

    fn collapse(&mut self) {
        let n = self.sel;
        if n == NONE {
            return;
        }
        if self.expanded.remove(&n) {
            self.rows_dirty = true;
        } else if self.tree.parent(n) != self.cwd {
            self.sel = self.tree.parent(n);
        }
    }

    fn drill_in(&mut self, n: Id) {
        if n == NONE || !self.tree.is_dir(n) || self.tree.kids(n).is_empty() {
            return;
        }
        self.cwd = n;
        self.sel = NONE;
        self.expanded.clear();
        self.scroll = 0;
        self.relayout(Kind::Nav, Hint::In(n));
    }

    fn go_up(&mut self) {
        if self.cwd == self.tree.root {
            self.say(color::SQUASH, "Top of the scan");
            return;
        }
        let prev = self.cwd;
        self.cwd = self.tree.parent(prev);
        self.sel = prev;
        self.expanded.clear();
        self.scroll = 0;
        self.relayout(Kind::Nav, Hint::Out(prev));
    }

    fn rescan(&mut self) {
        if self.scanning() {
            self.say(color::SQUASH, "A scan is already running");
        } else if self.scanner.is_some() {
            self.start_scan();
        }
    }

    fn ask_delete(&mut self) {
        if self.sel == NONE {
            return;
        }
        if self.scanning() {
            self.say(color::SQUASH, "Wait for the scan to finish before deleting");
            return;
        }
        self.modal = Some(Modal::Delete(self.sel));
    }

    fn confirm_delete(&mut self, n: Id) {
        self.modal = None;
        // `path` is None for anything not attached to the scan root.
        let path = if n == self.tree.root { None } else { self.tree.path(n) };
        let Some(path) = path else {
            self.say(color::RED, "That entry is no longer part of the scan");
            return;
        };
        let (tx, rx) = mpsc::channel();
        if self.cfg.dry_run {
            let _ = tx.send(Ok(()));
        } else {
            thread::spawn(move || {
                let _ = tx.send(remove_path(&path));
            });
        }
        let visual = !self.cfg.reduced && self.disp.contains_key(&(n as u64));
        self.dissolve = Some(Dissolve { node: n, t0: self.now, visual, rx, result: None });
    }

    fn finish_delete(&mut self, d: Dissolve) {
        match d.result {
            Some(Ok(())) => {
                let n = d.node;
                let p = self.tree.parent(n);
                if p == NONE {
                    return;
                }
                self.tree.sort_kids(p);
                let kids = self.tree.kids(p);
                let next = match kids.iter().position(|&k| k == n) {
                    Some(j) if j + 1 < kids.len() => kids[j + 1],
                    Some(j) if j > 0 => kids[j - 1],
                    _ if p != self.cwd => p,
                    _ => NONE,
                };
                if let Some(scanner) = &self.scanner {
                    let mut gone = self.tree.descendants(n);
                    gone.insert(n);
                    scanner.forget(&gone);
                }
                let size = self.tree.remove(n);
                self.freed += size;
                self.sel = next;
                self.expanded.remove(&n);
                self.relayout(Kind::Routine, Hint::None);
                self.say(color::GREEN, format!("Freed {}", fmt::size(size)));
            }
            Some(Err(e)) => self.say(color::RED, format!("Could not delete: {e}. Press r to rescan.")),
            None => {}
        }
    }

    pub fn key(&mut self, code: KeyCode, mods: KeyModifiers) {
        self.last_input = self.now;
        self.sel_auto = false;
        if code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        if self.dissolve.is_some() {
            return;
        }
        if let Some(m) = self.modal {
            match (m, code) {
                (Modal::Delete(n), KeyCode::Char('y' | 'Y')) => self.confirm_delete(n),
                (Modal::Delete(_), KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc) => self.modal = None,
                (Modal::Delete(_), _) => {}
                (Modal::Help, _) => self.modal = None,
            }
            return;
        }
        let files = self.focus == Focus::Files;
        let page = (self.files_in.h.max(2) - 1) as i64;
        match code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Left | KeyCode::Char('h') if files => self.collapse(),
            KeyCode::Right | KeyCode::Char('l') if files => self.expand(),
            KeyCode::Up | KeyCode::Char('k') if files => self.move_row(-1),
            KeyCode::Down | KeyCode::Char('j') if files => self.move_row(1),
            KeyCode::Left | KeyCode::Char('h') => self.move_map(-1, 0),
            KeyCode::Right | KeyCode::Char('l') => self.move_map(1, 0),
            KeyCode::Up | KeyCode::Char('k') => self.move_map(0, -1),
            KeyCode::Down | KeyCode::Char('j') => self.move_map(0, 1),
            KeyCode::PageUp => self.move_row(-page),
            KeyCode::PageDown => self.move_row(page),
            KeyCode::Home => self.move_row(i64::MIN / 2),
            KeyCode::End => self.move_row(i64::MAX / 2),
            KeyCode::Enter => self.drill_in(self.sel),
            KeyCode::Esc => self.go_up(),
            KeyCode::Tab | KeyCode::BackTab => self.focus = if files { Focus::Map } else { Focus::Files },
            KeyCode::Backspace | KeyCode::Delete | KeyCode::Char('d') => self.ask_delete(),
            KeyCode::Char('r') => self.rescan(),
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            _ => {}
        }
    }

    /// Click selects, a second click on the same entry opens it.
    pub fn click(&mut self, x: i32, y: i32) {
        self.last_input = self.now;
        self.sel_auto = false;
        if self.dissolve.is_some() {
            return;
        }
        if let Some(m) = self.modal {
            if m == Modal::Help {
                self.modal = None;
            }
            return;
        }
        let r = self.regions;
        if r.too_small {
            return;
        }
        if (!r.narrow || self.focus == Focus::Files) && self.files_in.contains(x, y) {
            self.focus = Focus::Files;
            if let Some(&(_, n)) = self.hit_rows.iter().find(|h| h.0 == y) {
                if n == self.sel {
                    self.drill_in(n)
                } else {
                    self.sel = n
                }
            }
        } else if (!r.narrow || self.focus == Focus::Map) && r.map_in.contains(x, y) {
            self.focus = Focus::Map;
            let hit = self.disp.values().map(|s| s.tile).find(|t| t.depth == 1 && !t.small() && t.r4().contains(x, y));
            if let Some(t) = hit {
                if t.node == self.sel {
                    self.drill_in(t.node)
                } else {
                    self.sel = t.node
                }
            }
        }
    }

    /// Handles a terminal event. Returns whether a redraw is needed.
    pub fn on_event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                self.key(k.code, k.modifiers);
                true
            }
            Event::Mouse(m) => {
                let (x, y) = (m.column as i32, m.row as i32);
                let over_files = (!self.regions.narrow || self.focus == Focus::Files) && self.files_in.contains(x, y);
                match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => self.click(x, y),
                    MouseEventKind::ScrollDown if over_files && self.modal.is_none() => self.move_row(3),
                    MouseEventKind::ScrollUp if over_files && self.modal.is_none() => self.move_row(-3),
                    _ => return false,
                }
                true
            }
            Event::Resize(..) => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::testing::{entry, tree};
    use crate::tree::NewEntry;

    const MB: u64 = 1 << 20;

    fn files(prefix: &str, ext: &str, sizes: &[u64]) -> Vec<NewEntry> {
        sizes.iter().enumerate().map(|(i, s)| entry(&format!("{prefix}{i}.{ext}"), s * MB, false)).collect()
    }

    /// ~/{VMs/{win.qcow2, deb.qcow2, iso/{a.iso}}, Videos/{k.mkv, c0..c3.mp4}, Docs/{30 pdfs}, notes.txt, 40 small logs}
    fn app() -> App {
        let t = tree(vec![
            (0, 1, vec![entry("VMs", 0, true), entry("Videos", 0, true), entry("Docs", 0, true), entry("notes.txt", MB, false)]),
            (1, 5, vec![entry("win.qcow2", 40_000 * MB, false), entry("deb.qcow2", 20_000 * MB, false), entry("iso", 0, true)]),
            (7, 8, vec![entry("a.iso", 5_000 * MB, false)]),
            (2, 9, {
                let mut v = vec![entry("k.mkv", 9_000 * MB, false)];
                v.extend(files("c", "mp4", &[4000, 3000, 2000, 1000]));
                v
            }),
            (3, 14, files("paper", "pdf", &[40; 30])),
            // Forty files that are each too small for a tile of their own.
            (0, 100, files("log", "txt", &[30; 40])),
        ]);
        let mut a = App::new(t, None, Config { reduced: false, dry_run: true, truecolor: true, aspect: 2.0 });
        a.resize(140, 36);
        a
    }

    fn name(a: &App, id: Id) -> String {
        if id == NONE {
            "-".into()
        } else {
            a.tree.name(id)
        }
    }

    fn settle(a: &mut App) {
        a.now += 5000.0;
        a.pump();
    }

    fn press(a: &mut App, code: KeyCode) {
        a.key(code, KeyModifiers::NONE);
    }

    #[test]
    fn regions_cover_the_screen_without_overlap() {
        for (cols, rows) in [(200, 60), (140, 36), (96, 30), (80, 24), (63, 30), (40, 20), (30, 12)] {
            let r = regions(cols, rows);
            assert!(!r.too_small, "{cols}x{rows}");
            for p in [r.files, r.map, r.insp] {
                assert!(p.x >= 0 && p.y >= 2 && p.x + p.w <= cols && p.y + p.h < rows, "{cols}x{rows} {p:?}");
                assert!(p.w >= 3 && p.h >= 3, "{cols}x{rows} {p:?}");
            }
            assert!(r.map_in.w >= 1 && r.map_in.h >= 1);
            if !r.narrow {
                assert_eq!(r.files.y + r.files.h, r.types.y);
                assert_eq!(r.map.y + r.map.h, r.insp.y);
                assert!(r.files.x + r.files.w < r.map.x);
            }
        }
        assert!(regions(29, 40).too_small);
        assert!(regions(80, 11).too_small);
    }

    #[test]
    fn first_layout_tiles_the_map_and_selects_the_largest() {
        let a = app();
        assert_eq!(name(&a, a.sel), "VMs");
        let m = a.regions.map_in;
        let top: Vec<Tile> = a.disp.values().map(|s| s.tile).filter(|t| t.depth == 1).collect();
        let area: i32 = top.iter().map(|t| t.w * t.h).sum();
        assert_eq!(area, m.w * m.h, "first-level tiles cover the map exactly");
        for t in a.disp.values().map(|s| s.tile) {
            assert!(t.x >= m.x && t.y >= m.y && t.x + t.w <= m.x + m.w && t.y + t.h <= m.y + m.h);
        }
        let vms = a.disp[&1].tile;
        assert!(vms.nested, "a large folder shows its contents");
        assert!(a.disp.values().any(|s| s.tile.depth == 2 && s.tile.parent == 1));
    }

    #[test]
    fn selection_follows_the_largest_entry_until_the_user_moves() {
        // Listings arrive smallest first and before the screen has a size,
        // as happens when a scan finishes within the first frame.
        let t = tree(vec![(0, 1, vec![entry("tiny.txt", MB, false), entry("big", 0, true), entry("mid.bin", 50 * MB, false)])]);
        let mut a = App::new(t, None, Config { reduced: true, dry_run: true, truecolor: true, aspect: 2.0 });
        a.relayout(Kind::Routine, Hint::None);
        assert_eq!(name(&a, a.sel), "mid.bin");
        a.tree.apply(Batch { dir: 2, first_id: 10, entries: vec![entry("huge.iso", 900 * MB, false)], error: false });
        a.resize(120, 30);
        assert_eq!(name(&a, a.sel), "big", "still automatic, so it tracks the new largest");
        press(&mut a, KeyCode::Down);
        let chosen = a.sel;
        a.tree.apply(Batch { dir: 0, first_id: 20, entries: vec![entry("later.bin", 5000 * MB, false)], error: false });
        a.relayout(Kind::Routine, Hint::None);
        assert_eq!(a.sel, chosen, "once moved, the selection stays put");
    }

    #[test]
    fn small_entries_fold_into_a_bucket() {
        let a = app();
        let bucket = a.disp.get(&small_key(0)).expect("the small files share one tile").tile;
        assert!(bucket.small());
        assert_eq!(bucket.hidden, 41, "forty logs and notes.txt");
        assert!(!a.disp.contains_key(&4) && !a.disp.contains_key(&100));
    }

    #[test]
    fn arrows_move_spatially_and_come_back() {
        let mut a = app();
        press(&mut a, KeyCode::Right);
        let right = a.sel;
        assert_ne!(name(&a, right), "VMs");
        let (o, t) = (a.disp[&1].tile, a.disp[&(right as u64)].tile);
        assert!(t.x >= o.x + o.w, "the tile to the right starts past the old one");
        press(&mut a, KeyCode::Left);
        assert_eq!(name(&a, a.sel), "VMs");
        press(&mut a, KeyCode::Left);
        assert_eq!(name(&a, a.sel), "VMs", "nothing further left");
    }

    #[test]
    fn enter_and_escape_walk_the_tree() {
        let mut a = app();
        press(&mut a, KeyCode::Enter);
        assert_eq!(name(&a, a.cwd), "VMs");
        assert_eq!(name(&a, a.sel), "win.qcow2");
        assert!(a.anim.dur > 0.0 && !a.leaving.is_empty(), "drilling in animates");
        settle(&mut a);
        press(&mut a, KeyCode::Enter);
        assert_eq!(name(&a, a.cwd), "VMs", "a file cannot be entered");
        press(&mut a, KeyCode::Esc);
        assert_eq!(a.cwd, a.tree.root);
        assert_eq!(name(&a, a.sel), "VMs", "coming back selects the folder we left");
        press(&mut a, KeyCode::Esc);
        assert_eq!(a.cwd, a.tree.root);
        assert!(a.flash.as_ref().unwrap().msg.contains("Top"));
    }

    #[test]
    fn drill_in_starts_children_inside_the_folder_tile() {
        let mut a = app();
        let r0 = a.disp[&1].tile;
        press(&mut a, KeyCode::Enter);
        for s in a.disp.values().filter(|s| s.tile.depth == 1) {
            let f = s.from;
            assert!(f.x >= r0.x as f64 - 0.01 && f.y >= r0.y as f64 - 0.01);
            assert!(f.x + f.w <= (r0.x + r0.w) as f64 + 0.01 && f.y + f.h <= (r0.y + r0.h) as f64 + 0.01, "{f:?} starts inside {r0:?}");
        }
    }

    #[test]
    fn file_list_expands_in_place_and_shares_the_selection() {
        let mut a = app();
        press(&mut a, KeyCode::Tab);
        assert_eq!(a.focus, Focus::Files);
        press(&mut a, KeyCode::Right);
        a.ensure_rows();
        assert_eq!(a.rows.iter().map(|r| r.1).collect::<Vec<_>>()[..4], [0, 1, 1, 1], "VMs opened in place");
        press(&mut a, KeyCode::Right);
        assert_eq!(name(&a, a.sel), "win.qcow2");
        assert_eq!(a.sel_key(), Some(a.sel as u64), "the row's own nested tile is highlighted");
        press(&mut a, KeyCode::Left);
        assert_eq!(name(&a, a.sel), "VMs");
        press(&mut a, KeyCode::Left);
        a.ensure_rows();
        assert!(a.rows.iter().all(|r| r.1 == 0), "collapsed again");
        press(&mut a, KeyCode::End);
        assert_eq!(name(&a, a.sel), "notes.txt");
        assert_eq!(a.sel_key(), Some(small_key(0)), "a small entry highlights the bucket");
    }

    #[test]
    fn delete_asks_first_and_updates_totals() {
        let mut a = app();
        let before = a.tree.size(0);
        let vms = a.tree.size(1);
        press(&mut a, KeyCode::Backspace);
        assert_eq!(a.modal, Some(Modal::Delete(1)));
        press(&mut a, KeyCode::Char('n'));
        assert_eq!(a.modal, None);
        assert_eq!(a.tree.size(0), before, "cancelling changes nothing");
        press(&mut a, KeyCode::Char('d'));
        press(&mut a, KeyCode::Char('y'));
        assert!(a.dissolve.is_some());
        a.now += 100.0;
        a.pump();
        assert!(a.dissolve.is_some(), "the tile is still dissolving");
        press(&mut a, KeyCode::Right);
        assert_eq!(a.sel, 1, "input waits for the deletion");
        settle(&mut a);
        assert!(a.dissolve.is_none());
        assert_eq!(a.freed, vms);
        assert_eq!(a.tree.size(0), before - vms);
        assert_eq!(name(&a, a.sel), "Videos");
        assert!(!a.disp.contains_key(&1));
        assert!(a.flash.as_ref().unwrap().msg.starts_with("Freed"));
    }

    #[test]
    fn deleting_never_targets_the_root_or_detached_nodes() {
        let mut a = app();
        a.confirm_delete(0);
        assert!(a.dissolve.is_none());
        a.tree.remove(2);
        a.confirm_delete(9);
        assert!(a.dissolve.is_none());
        assert!(a.flash.as_ref().unwrap().msg.contains("no longer"));
    }

    #[test]
    fn reduced_motion_settles_at_once() {
        let mut a = app();
        a.cfg.reduced = true;
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.anim.dur, 0.0);
        assert!(a.leaving.is_empty());
        a.now += 1.0;
        press(&mut a, KeyCode::Esc);
        assert!(!a.busy() || a.flash.is_some());
    }

    #[test]
    fn resizing_keeps_the_selection_and_relays_out() {
        let mut a = app();
        press(&mut a, KeyCode::Right);
        let sel = a.sel;
        a.resize(50, 24);
        assert!(a.regions.narrow);
        assert_eq!(a.sel, sel);
        let m = a.regions.map_in;
        let area: i32 = a.disp.values().map(|s| s.tile).filter(|t| t.depth == 1).map(|t| t.w * t.h).sum();
        assert_eq!(area, m.w * m.h);
        a.resize(20, 5);
        assert!(a.regions.too_small && a.disp.is_empty());
    }

    #[test]
    fn mouse_click_selects_then_opens() {
        let mut a = app();
        let t = a.disp[&2].tile;
        a.click(t.x + 1, t.y + 1);
        assert_eq!(name(&a, a.sel), "Videos");
        assert_eq!(a.cwd, 0);
        a.click(t.x + 1, t.y + 1);
        assert_eq!(name(&a, a.cwd), "Videos");
    }
}
