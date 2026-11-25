//! Painting. Everything is drawn into the cell grid and flushed once.

use ratatui::Frame;

use crate::app::{ease, small_key, App, Focus, Modal, Tile, FLASH_SHOWN, SHEEN, T_DISSOLVE, T_FLASH};
use crate::cells::{bold, seg, Cells, Seg, R4, SEAM_BOTTOM, SEAM_LEFT};
use crate::color::{self, mix, Rgb};
use crate::fmt::{self, fit, width};
use crate::tree::{Id, NONE, NO_EXT};
use crate::treemap::RectF;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    app.resize(area.width as i32, area.height as i32);
    let mut c = std::mem::take(&mut app.cells);
    c.reset(area.width as i32, area.height as i32);
    app.paint(&mut c);
    c.flush(f.buffer_mut(), app.cfg.truecolor);
    app.cells = c;
}

fn sep() -> Seg {
    seg(" · ", color::FAINT)
}

/// Deterministic per-cell threshold for the dissolve: one checker colour
/// leaves first, with noise inside each half.
fn cell_noise(x: i32, y: i32) -> f64 {
    let mut h = ((x as u32).wrapping_mul(374_761_393).wrapping_add((y as u32).wrapping_mul(668_265_263))) ^ 0x0D3E_1E7E;
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    ((h ^ (h >> 16)) as f64 / 4_294_967_296.0) * 0.6 + if (x + y) & 1 == 1 { 0.4 } else { 0.0 }
}

/// Integer cell rectangle of a (possibly mid-animation) tile.
fn snap(r: RectF) -> R4 {
    let (x0, y0) = (r.x.round() as i32, r.y.round() as i32);
    R4 { x: x0, y: y0, w: (r.x + r.w).round() as i32 - x0, h: (r.y + r.h).round() as i32 - y0 }
}

fn clip(r: R4, m: R4) -> R4 {
    let (x0, y0) = (r.x.max(m.x), r.y.max(m.y));
    let (x1, y1) = ((r.x + r.w).min(m.x + m.w), (r.y + r.h).min(m.y + m.h));
    R4 { x: x0, y: y0, w: (x1 - x0).max(0), h: (y1 - y0).max(0) }
}

impl App {
    pub(crate) fn paint(&mut self, c: &mut Cells) {
        let r = self.regions;
        if r.too_small {
            c.put(0, 0, &fit("Terminal too small. Needs 30 x 12.", c.w), color::SQUASH, None, true);
            return;
        }
        self.paint_header(c);
        if r.narrow {
            if self.focus == Focus::Map {
                let inner = c.pane(r.map, "STORAGE MAP", true);
                self.paint_map(c, inner);
            } else {
                self.paint_files(c, r.files, true);
            }
            self.paint_strip(c, r.strip_y);
        } else {
            self.paint_files(c, r.files, self.focus == Focus::Files);
            self.paint_types(c, r.types);
            let inner = c.pane(r.map, "STORAGE MAP", self.focus == Focus::Map);
            self.paint_map(c, inner);
            if self.disp.contains_key(&small_key(self.cwd)) {
                let s = " ░░ small entries ";
                c.put(r.map.x + r.map.w - 2 - width(s), r.map.y + r.map.h - 1, s, color::DIM, None, false);
            }
        }
        self.paint_inspector(c, r.insp);
        self.paint_footer(c);
        if let Some(f) = &self.flash {
            let t = (self.now - f.t0) / T_FLASH;
            if t < 1.0 && !self.cfg.reduced {
                c.tint_rows(2, f.color, t as f32);
            }
        }
        if self.flash.as_ref().is_some_and(|f| self.now - f.t0 > FLASH_SHOWN) {
            self.flash = None;
        }
        if let Some(m) = self.modal {
            self.paint_modal(c, m);
        }
    }

    fn paint_header(&self, c: &mut Cells) {
        let cols = c.w;
        let scanning = self.scanning();
        let chip = if scanning { " ◆ SCANNING " } else { " ◆ COMPLETE " };
        let dry = if self.cfg.dry_run { " DRY RUN " } else { "" };
        let mut x = c.put(0, 0, " RADU ", color::INK, Some(color::SQUASH), true);
        let room = cols - x - 3 - width(chip) - width(dry);
        x = c.put(x + 1, 0, &fmt::tail(&self.tree.dir_label(self.cwd), room.max(3)), color::FG, None, true);
        if x + 1 + width(chip) <= cols {
            let (fg, bg) = if scanning { (color::SQUASH, color::BORDER) } else { (color::INK, color::GREEN) };
            x = c.put(x + 1, 0, chip, fg, Some(bg), true);
        }
        if !dry.is_empty() && x + 1 + width(dry) <= cols {
            x = c.put(x + 1, 0, dry, color::INK, Some(color::RED), true);
        }
        // A flash message sits at the right of the first row, or takes the
        // summary row when it is too long to fit there.
        if let Some(f) = self.flash.as_ref().filter(|f| self.now - f.t0 < FLASH_SHOWN) {
            let mx = cols - width(&f.msg) - 1;
            if mx > x + 1 {
                c.put(mx, 0, &f.msg, f.color, None, true);
            } else {
                c.put(1, 1, &fit(&f.msg, cols - 2), f.color, None, true);
                return;
            }
        }
        let (here, root) = (self.tree.node(self.cwd), self.tree.node(self.tree.root));
        let mut parts = if scanning {
            vec![
                seg("Measuring ", color::DIM),
                bold(fmt::size(root.size), color::FG),
                sep(),
                seg(format!("{} files so far", fmt::num(root.count as u64)), color::DIM),
            ]
        } else {
            vec![
                seg("This folder ", color::DIM),
                bold(fmt::size(here.size), color::FG),
                sep(),
                seg(format!("{} files", fmt::num(here.count as u64)), color::DIM),
                sep(),
                seg("Scan total ", color::DIM),
                bold(fmt::size(root.size), color::FG),
                sep(),
                seg("Freed ", color::DIM),
                bold(fmt::size(self.freed), if self.freed > 0 { color::GREEN } else { color::FG }),
            ]
        };
        if self.tree.errors > 0 {
            parts.push(sep());
            parts.push(seg(format!("{} unreadable", fmt::num(self.tree.errors)), color::RED));
        }
        c.segs(1, 1, cols - 1, &parts);
    }

    fn type_name(&self, ext: u16, none: &'static str) -> String {
        if ext == NO_EXT {
            none.to_string()
        } else {
            self.tree.ext_name(ext).to_string()
        }
    }

    /// Share of `n` within its parent, by size.
    fn share(&self, n: Id) -> f64 {
        let p = self.tree.parent(n);
        if p == NONE || self.tree.size(p) == 0 {
            0.0
        } else {
            self.tree.size(n) as f64 / self.tree.size(p) as f64
        }
    }

    fn paint_files(&mut self, c: &mut Cells, p: R4, on: bool) {
        let inner = c.pane(p, "FILES", on);
        self.files_in = inner;
        self.hit_rows.clear();
        self.ensure_rows();
        if self.rows.is_empty() {
            let msg = if self.scanning() { "Reading…" } else { "Empty folder" };
            c.put(inner.x + 1, inner.y, &fit(msg, inner.w - 2), color::DIM, None, false);
            return;
        }
        let visible = inner.h.max(0) as usize;
        if let Some(i) = self.rows.iter().position(|r| r.0 == self.sel) {
            if i < self.scroll {
                self.scroll = i;
            }
            if i >= self.scroll + visible {
                self.scroll = i + 1 - visible;
            }
        }
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(visible));
        let (size_w, pct_w) = (7, 5);
        let bar_w = if inner.w >= 44 {
            10
        } else if inner.w >= 36 {
            6
        } else {
            0
        };
        let name_end = inner.x + inner.w - size_w - pct_w - if bar_w > 0 { bar_w + 1 } else { 0 };
        let sel_bg = if on { mix(color::BG, color::SQUASH, 0.24) } else { mix(color::BG, color::FG, 0.12) };
        for r in 0..visible.min(self.rows.len() - self.scroll) {
            let (n, depth) = self.rows[self.scroll + r];
            let y = inner.y + r as i32;
            let is_sel = n == self.sel;
            let is_dir = self.tree.is_dir(n);
            if is_sel {
                c.fill(R4 { x: inner.x, y, w: inner.w, h: 1 }, sel_bg);
            }
            let tc = self.palette.color(self.tree.dominant(n), 0.76);
            let x = inner.x + 1 + depth as i32 * 2;
            if x < name_end - 3 {
                let unreadable = self.tree.node(n).dir.as_ref().is_some_and(|d| d.error);
                let icon = if !is_dir {
                    "■"
                } else if self.expanded.contains(&n) {
                    "▾"
                } else {
                    "▸"
                };
                c.put(x, y, icon, if unreadable { color::RED } else { tc }, None, false);
                let label = self.tree.name(n) + if is_dir { "/" } else { "" };
                let fg = if is_sel {
                    color::WHITE
                } else if is_dir {
                    color::FG
                } else {
                    color::SOFT
                };
                c.put(x + 2, y, &fit(&label, name_end - x - 3), fg, None, is_dir);
            }
            let share = self.share(n);
            let mut cx = name_end;
            if bar_w > 0 {
                paint_bar(c, cx, y, bar_w, share, tc, if is_sel { mix(sel_bg, color::INK, 0.4) } else { color::TRACK });
                cx += bar_w + 1;
            }
            let (ps, ss) = (fmt::pct(share), fmt::size(self.tree.size(n)));
            c.put(cx + pct_w - 1 - width(&ps), y, &ps, if is_sel { color::FG } else { color::DIM }, None, false);
            c.put(inner.x + inner.w - 1 - width(&ss), y, &ss, if is_sel { color::WHITE } else { color::SOFT }, None, false);
            self.hit_rows.push((y, n));
        }
        let more = self.rows.len() as i64 - self.scroll as i64 - visible as i64;
        if more > 0 {
            let s = format!(" +{} more ", fmt::num(more as u64));
            c.put(p.x + p.w - 2 - width(&s), p.y + p.h - 1, &s, color::DIM, None, false);
        }
    }

    fn sel_ext(&self) -> Option<u16> {
        (self.sel != NONE).then(|| self.tree.dominant(self.sel))
    }

    fn paint_types(&mut self, c: &mut Cells, p: R4) {
        let inner = c.pane(p, "TYPES", false);
        self.ensure_legend();
        if self.legend.is_empty() {
            c.put(inner.x + 1, inner.y, &fit("Nothing measured yet", inner.w - 2), color::DIM, None, false);
            return;
        }
        let fits = inner.h.max(0) as usize;
        let shown = if self.legend.len() > fits { fits.saturating_sub(1) } else { self.legend.len() };
        let hot_ext = self.sel_ext();
        let total = self.legend_total.max(1) as f64;
        let line = |c: &mut Cells, y: i32, swatch: Rgb, name: &str, v: u64, hot: bool| {
            c.put(inner.x + 1, y, "■", swatch, None, false);
            c.put(inner.x + 3, y, &fit(name, inner.w - 20), if hot { color::WHITE } else { color::SOFT }, None, hot);
            let (ss, ps) = (fmt::size(v), fmt::pct(v as f64 / total));
            c.put(inner.x + inner.w - 7 - width(&ss), y, &ss, if hot { color::WHITE } else { color::SOFT }, None, false);
            c.put(inner.x + inner.w - 1 - width(&ps), y, &ps, if hot { color::FG } else { color::DIM }, None, false);
        };
        let mut covered = 0;
        for (i, &(ext, v)) in self.legend.iter().take(shown).enumerate() {
            line(c, inner.y + i as i32, self.palette.color(ext, 0.76), &self.type_name(ext, "no extension"), v, Some(ext) == hot_ext);
            covered += v;
        }
        if self.legend.len() > shown && fits > 0 {
            line(c, inner.y + shown as i32, color::FAINT, "other", self.legend_total - covered, false);
        }
    }

    /// One-line legend used when the terminal is too narrow for the pane.
    fn paint_strip(&mut self, c: &mut Cells, y: i32) {
        self.ensure_legend();
        let hot = self.sel_ext();
        let mut x = 1;
        for &(ext, _) in &self.legend {
            let name = self.type_name(ext, "none");
            if x + width(&name) + 3 > c.w {
                break;
            }
            c.put(x, y, "■", self.palette.color(ext, 0.76), None, false);
            x = c.put(x + 2, y, &name, if Some(ext) == hot { color::WHITE } else { color::DIM }, None, false) + 2;
        }
    }

    fn paint_inspector(&self, c: &mut Cells, p: R4) {
        let inner = c.pane(p, "SELECTED ITEM", false);
        let (x, max_x) = (inner.x + 1, inner.x + inner.w - 1);
        let n = self.sel;
        if n == NONE {
            let msg = if self.scanning() { "Reading…" } else { "Nothing here. Esc goes up." };
            c.put(x, inner.y, &fit(msg, inner.w - 2), color::DIM, None, false);
            return;
        }
        let is_dir = self.tree.is_dir(n);
        let name = self.tree.name(n) + if is_dir { "/" } else { "" };
        let prefix = fmt::tail(&self.tree.dir_label(self.tree.parent(n)), (inner.w - 2 - width(&name)).max(0));
        c.segs(x, inner.y, max_x, &[seg(prefix, color::DIM), bold(name, color::WHITE)]);
        let v = self.tree.size(n);
        let of_scan = v as f64 / self.tree.size(self.tree.root).max(1) as f64;
        c.segs(
            x,
            inner.y + 1,
            max_x,
            &[
                seg("Space used ", color::DIM),
                bold(fmt::size(v), color::FG),
                sep(),
                seg(format!("{} of parent", fmt::pct(self.share(n))), color::DIM),
                sep(),
                seg(format!("{} of scan", fmt::pct(of_scan)), color::DIM),
            ],
        );
        let ext = self.tree.dominant(n);
        let swatch = seg("■ ", self.palette.color(ext, 0.76));
        let tname = seg(self.type_name(ext, "no extension"), color::SOFT);
        let mut kind = if is_dir {
            let unreadable = self.tree.node(n).dir.as_ref().is_some_and(|d| d.error);
            let mut k =
                vec![seg("Folder", color::FG), sep(), seg(format!("{} files", fmt::num(self.tree.node(n).count as u64)), color::DIM)];
            if unreadable {
                k.extend([sep(), seg("could not be read fully", color::RED)]);
            } else if self.tree.node(n).count > 0 {
                k.extend([sep(), seg("mostly ", color::DIM), swatch, tname]);
            }
            k
        } else {
            vec![seg("File", color::FG), sep(), swatch, tname]
        };
        let mut acts = Vec::new();
        if is_dir && !self.tree.kids(n).is_empty() {
            acts.extend([bold("Enter", color::SQUASH), seg(" open", color::DIM), sep()]);
        }
        acts.extend([bold("Bksp", color::SQUASH), seg(" delete", color::DIM)]);
        if inner.h >= 4 {
            c.segs(x, inner.y + 2, max_x, &kind);
            c.segs(x, inner.y + 3, max_x, &acts);
        } else if inner.h == 3 {
            kind.push(sep());
            kind.extend(acts);
            c.segs(x, inner.y + 2, max_x, &kind);
        }
    }

    fn paint_footer(&self, c: &mut Cells) {
        let y = c.h - 1;
        let hints: &[(&str, &str)] = match (self.modal, self.focus) {
            (Some(Modal::Delete(_)), _) => &[("y", "delete"), ("n", "cancel")],
            (Some(Modal::Help), _) => &[("any key", "close")],
            (None, Focus::Map) => &[
                ("arrows/hjkl", "move"),
                ("Enter", "open"),
                ("Esc", "up"),
                ("Tab", "files"),
                ("Bksp", "delete"),
                ("r", "rescan"),
                ("?", "help"),
                ("q", "quit"),
            ],
            (None, Focus::Files) => &[
                ("↑↓", "move"),
                ("→", "expand"),
                ("←", "collapse"),
                ("Enter", "open"),
                ("Esc", "up"),
                ("Tab", "map"),
                ("Bksp", "delete"),
                ("?", "help"),
                ("q", "quit"),
            ],
        };
        let mut x = 1;
        for (k, d) in hints {
            if x + width(k) + 1 + width(d) > c.w - 1 {
                break;
            }
            x = c.put(x, y, k, color::SQUASH, None, true);
            x = c.put(x + 1, y, d, color::DIM, None, false) + 2;
        }
    }

    fn paint_modal(&self, c: &mut Cells, m: Modal) {
        let key = |k: &str, d: &str| vec![bold(format!("{k:<15}"), color::SQUASH), seg(d, color::FG)];
        let (title, edge, w, lines): (&str, Rgb, i32, Vec<Vec<Seg>>) = match m {
            Modal::Delete(n) => {
                let w = (c.w - 2).min(56);
                let is_dir = self.tree.is_dir(n);
                let name = self.tree.name(n) + if is_dir { "/" } else { "" };
                let mut frees = vec![seg("Frees ", color::DIM), bold(fmt::size(self.tree.size(n)), color::GREEN)];
                if is_dir {
                    frees.push(seg(format!(" across {} files", fmt::num(self.tree.node(n).count as u64)), color::DIM));
                }
                let warn = if self.cfg.dry_run {
                    seg("Dry run. Nothing on disk is touched.", color::DIM)
                } else {
                    seg("This is permanent. There is no trash.", color::RED)
                };
                let lines = vec![
                    vec![seg("Delete ", color::FG), bold(fit(&name, w - 15), color::WHITE), seg("?", color::FG)],
                    frees,
                    vec![warn],
                    vec![],
                    vec![bold("y", color::RED), seg(" delete     ", color::FG), bold("n", color::SQUASH), seg(" cancel", color::FG)],
                ];
                (" DELETE ", color::RED, w, lines)
            }
            Modal::Help => {
                let lines = vec![
                    key("arrows / hjkl", "move between tiles or rows"),
                    key("Enter", "open the selected folder"),
                    key("Esc", "go up to the parent folder"),
                    key("Tab", "switch between map and files"),
                    key("→ / ←", "expand or collapse in the file list"),
                    key("PgUp PgDn", "page through the file list"),
                    key("Bksp or d", "delete, asks first"),
                    key("r", "rescan this folder"),
                    key("q", "quit"),
                ];
                (" KEYS ", color::SQUASH, (c.w - 2).min(56), lines)
            }
        };
        let h = lines.len() as i32 + 4;
        let r = R4 { x: (c.w - w) / 2, y: ((c.h - h) / 2).max(0), w, h };
        c.fill(r, color::LIFT);
        c.frame(r, edge, false);
        c.put(r.x + 2, r.y, title, color::INK, Some(edge), true);
        for (i, line) in lines.iter().enumerate() {
            c.segs(r.x + 3, r.y + 2 + i as i32, r.x + w - 2, line);
        }
    }

    // ---------- map ----------

    fn tile_label(&self, c: &mut Cells, t: &Tile, r: R4, m: R4) {
        if r.w < 4 || r.h < 1 {
            return;
        }
        c.clip = Some(clip(r, m));
        let name = if t.small() {
            format!("+{} small", fmt::num(t.hidden as u64))
        } else {
            self.tree.name(t.node) + if self.tree.is_dir(t.node) { "/" } else { "" }
        };
        let (size, pc) = (fmt::size(t.v), fmt::pct(t.share as f64));
        let centre = |s: &str| r.x + (r.w - width(s)) / 2;
        if t.nested {
            let full = format!("{name}  {size} · {pc}");
            let text = if width(&full) <= r.w - 3 { full } else { fit(&name, r.w - 3) };
            c.put(r.x + 2, r.y, &text, t.ink, None, true);
        } else if t.depth == 1 {
            let (l1, l2) = (fit(&name, r.w - 2), format!("{size} · {pc}"));
            if r.h >= 2 && width(&l2) <= r.w - 2 {
                let y = r.y + (r.h - 2) / 2;
                c.put(centre(&l1), y, &l1, t.ink, None, true);
                c.put(centre(&l2), y + 1, &l2, mix(t.ink, t.bg, 0.3), None, false);
            } else {
                c.put(centre(&l1), r.y + (r.h - 1) / 2, &l1, t.ink, None, true);
            }
        } else if r.w >= 5 {
            c.put(r.x + 1, r.y, &fit(&name, r.w - 2), mix(t.ink, t.bg, 0.12), None, false);
            if r.h >= 2 && r.w >= 7 {
                c.put(r.x + 1, r.y + 1, &fit(&size, r.w - 2), mix(t.ink, t.bg, 0.4), None, false);
            }
        }
        c.clip = None;
    }

    /// How far a tile has dissolved, or `None` when it is not being deleted.
    fn dissolve_progress(&self, t: &Tile) -> Option<f64> {
        let d = self.dissolve.as_ref().filter(|d| d.visual)?;
        let hit = t.node == d.node || t.parent == d.node;
        hit.then(|| (self.now - d.t0) / T_DISSOLVE)
    }

    fn paint_tile(&self, c: &mut Cells, t: &Tile, rf: RectF, alpha: f64, m: R4, ghost: bool) {
        let r = snap(rf);
        let v = clip(r, m);
        if v.w == 0 || v.h == 0 {
            return;
        }
        let bg = if alpha < 1.0 { mix(color::BG, t.bg, alpha as f32) } else { t.bg };
        let fg = if t.small() { mix(bg, color::DIM, 0.45) } else { t.ink };
        let dp = self.dissolve_progress(t);
        for y in v.y..v.y + v.h {
            for x in v.x..v.x + v.w {
                if dp.is_some_and(|p| cell_noise(x, y) < p) {
                    c.set(x, y, ' ', color::FG, color::BG, 0);
                    continue;
                }
                let seam = if x == r.x && r.x > m.x { SEAM_LEFT } else { 0 }
                    | if y == r.y + r.h - 1 && r.y + r.h < m.y + m.h { SEAM_BOTTOM } else { 0 };
                c.set(x, y, if t.small() { '░' } else { ' ' }, fg, bg, seam);
            }
        }
        if !ghost && alpha > 0.55 && dp.is_none() {
            self.tile_label(c, t, r, m);
        }
    }

    /// Lifts the selected tile, runs the sheen across it once after the
    /// last input, and outlines it.
    fn paint_selection(&self, c: &mut Cells, m: R4, e: f64) {
        let Some(s) = self.sel_key().and_then(|k| self.disp.get(&k)) else { return };
        if self.dissolve.as_ref().is_some_and(|d| d.node == self.sel) {
            return;
        }
        let r = snap(s.from.lerp(s.tile.rect(), e));
        let v = clip(r, m);
        if v.w == 0 || v.h == 0 {
            return;
        }
        let since = self.now - self.last_input;
        let live = !self.cfg.reduced && since < SHEEN;
        let span = (r.w as f64 * 0.5 + r.h as f64).max(1.0);
        let head = -0.25 + 1.5 * since / SHEEN;
        for y in v.y..v.y + v.h {
            for x in v.x..v.x + v.w {
                let mut lift = 0.1;
                if live {
                    let d = ((x - r.x) as f64 * 0.5 + (y - r.y) as f64) / span - head;
                    lift += 0.36 * (-d * d / 0.022).exp();
                }
                if let Some(bg) = c.bg_at(x, y) {
                    c.set_bg(x, y, mix(bg, color::WHITE, lift as f32));
                }
            }
        }
        let heavy = self.focus == Focus::Map;
        c.clip = Some(v);
        c.frame(r, if heavy { color::WHITE } else { mix(color::WHITE, color::BG, 0.3) }, heavy);
        c.clip = None;
        self.tile_label(c, &s.tile, r, m);
    }

    fn paint_map(&mut self, c: &mut Cells, m: R4) {
        let t = self.anim_t();
        if t >= 1.0 {
            self.leaving.clear();
        }
        let e = ease(t);
        if self.disp.is_empty() && self.leaving.is_empty() {
            let (msg, fg, strong) = if self.scanning() {
                // The empty scan field: a quiet dot grid until sizes arrive.
                for y in (m.y..m.y + m.h).step_by(2) {
                    for x in (m.x + 1 + ((y / 2) & 1) * 2..m.x + m.w).step_by(4) {
                        c.put(x, y, "·", color::FAINT, None, false);
                    }
                }
                (format!(" Measuring {} ", self.tree.dir_label(self.cwd)), color::SQUASH, true)
            } else if self.tree.kids(self.cwd).is_empty() {
                ("Empty folder".to_string(), color::DIM, false)
            } else {
                ("Nothing here takes up space".to_string(), color::DIM, false)
            };
            let msg = fit(&msg, m.w);
            c.put(m.x + (m.w - width(&msg)) / 2, m.y + m.h / 2, &msg, fg, Some(color::BG), strong);
            return;
        }
        for l in &self.leaving {
            self.paint_tile(c, &l.tile, l.from.lerp(l.to, e), 1.0 - e, m, true);
        }
        for depth in 1..=2 {
            for s in self.disp.values().filter(|s| s.tile.depth == depth) {
                self.paint_tile(c, &s.tile, s.from.lerp(s.tile.rect(), e), if s.fade { e } else { 1.0 }, m, false);
            }
        }
        self.paint_selection(c, m, e);
    }
}

/// Share bar with eighth-block precision.
fn paint_bar(c: &mut Cells, x: i32, y: i32, w: i32, frac: f64, fg: Rgb, track: Rgb) {
    const EIGHTHS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
    let cells = frac.clamp(0.0, 1.0) * w as f64;
    let full = cells.floor() as i32;
    let rem = ((cells - full as f64) * 8.0).round() as usize;
    for i in 0..w {
        if i < full {
            c.set(x + i, y, ' ', fg, fg, 0);
        } else if i == full && rem > 0 {
            c.set(x + i, y, EIGHTHS[rem - 1], fg, track, 0);
        } else {
            c.set(x + i, y, ' ', fg, track, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Config;
    use crate::tree::testing::{entry, tree};
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    const MB: u64 = 1 << 20;

    fn app(cols: i32, rows: i32) -> App {
        let t = tree(vec![
            (0, 1, vec![entry("VMs", 0, true), entry("Videos", 0, true), entry("notes.txt", MB, false), entry("empty", 0, true)]),
            (1, 5, vec![entry("win11.qcow2", 40_000 * MB, false), entry("debian.qcow2", 20_000 * MB, false)]),
            (2, 7, vec![entry("keynote.mkv", 9_000 * MB, false), entry("clip.mp4", 4_000 * MB, false)]),
            (0, 50, (0..40).map(|i| entry(&format!("log{i}.txt"), 30 * MB, false)).collect()),
        ]);
        let mut a = App::new(t, None, Config { reduced: false, dry_run: true, truecolor: true, aspect: 2.0 });
        a.resize(cols, rows);
        a.now = 10_000.0;
        a
    }

    fn screen(a: &mut App, cols: i32, rows: i32) -> String {
        let mut c = Cells::default();
        c.reset(cols, rows);
        a.paint(&mut c);
        c.dump()
    }

    #[test]
    fn wide_layout_shows_every_pane() {
        let mut a = app(140, 36);
        let s = screen(&mut a, 140, 36);
        for want in [
            "RADU",
            "COMPLETE",
            "DRY▒RUN",
            "FILES",
            "TYPES",
            "STORAGE▒MAP",
            "SELECTED▒ITEM",
            "VMs/",
            "Videos/",
            ".qcow2",
            "win11.qcow2",
            "Space used 58.6G",
            "of scan",
            "arrows/hjkl",
            "small entries",
        ] {
            assert!(s.contains(want), "missing {want:?} in\n{s}");
        }
        assert!(s.contains("┏") && s.contains("┛"), "the selected tile is outlined");
        assert_eq!(s.lines().count(), 36);
    }

    #[test]
    fn narrow_layout_shows_one_pane_at_a_time() {
        let mut a = app(48, 24);
        let s = screen(&mut a, 48, 24);
        assert!(s.contains("STORAGE▒MAP") && !s.contains("FILES") && !s.contains("TYPES"), "{s}");
        assert!(s.contains("■ .qcow2"), "the legend collapses to a strip\n{s}");
        a.key(KeyCode::Tab, KeyModifiers::NONE);
        let s = screen(&mut a, 48, 24);
        assert!(s.contains("FILES") && !s.contains("STORAGE▒MAP"), "{s}");
    }

    #[test]
    fn nothing_is_drawn_outside_the_grid_at_any_size() {
        for (cols, rows) in [(30, 12), (31, 13), (64, 12), (63, 40), (96, 16), (250, 70), (10, 4)] {
            let mut a = app(cols, rows);
            let s = screen(&mut a, cols, rows);
            assert_eq!(s.lines().count(), rows as usize);
            assert!(s.lines().all(|l| width(l) <= cols), "{cols}x{rows}\n{s}");
        }
    }

    #[test]
    fn modals_and_messages_render() {
        let mut a = app(120, 30);
        a.key(KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(screen(&mut a, 120, 30).contains("KEYS"));
        a.key(KeyCode::Char('x'), KeyModifiers::NONE);
        a.key(KeyCode::Backspace, KeyModifiers::NONE);
        let s = screen(&mut a, 120, 30);
        assert!(s.contains("DELETE") && s.contains("Delete▒VMs/?") && s.contains("Dry▒run"), "{s}");
        a.key(KeyCode::Char('y'), KeyModifiers::NONE);
        a.now += 5_000.0;
        a.pump();
        let s = screen(&mut a, 120, 30);
        assert!(s.contains("Freed 58.6G") && !s.contains("VMs/"), "{s}");
    }

    #[test]
    fn deleting_dissolves_the_tile_cell_by_cell() {
        let mut a = app(120, 30);
        let t = a.disp[&1].tile;
        let filled = |a: &mut App| {
            let mut c = Cells::default();
            c.reset(120, 30);
            a.paint(&mut c);
            (t.y..t.y + t.h).flat_map(|y| (t.x..t.x + t.w).map(move |x| (x, y))).filter(|&(x, y)| c.bg_at(x, y) != Some(color::BG)).count()
        };
        let all = filled(&mut a);
        assert_eq!(all, (t.w * t.h) as usize);
        a.key(KeyCode::Backspace, KeyModifiers::NONE);
        a.key(KeyCode::Char('y'), KeyModifiers::NONE);
        a.now += T_DISSOLVE / 2.0;
        let half = filled(&mut a);
        assert!(half > 0 && half < all, "{half} of {all} cells left halfway");
    }

    #[test]
    fn empty_and_unsized_folders_say_so() {
        let mut a = app(120, 30);
        a.key(KeyCode::Tab, KeyModifiers::NONE);
        a.key(KeyCode::End, KeyModifiers::NONE);
        assert_eq!(a.tree.name(a.sel), "empty");
        a.key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(a.cwd, a.tree.root, "an empty folder cannot be entered");
        assert!(screen(&mut a, 120, 30).contains("Folder · 0 files"));
    }

    #[test]
    fn wide_characters_keep_the_grid_aligned() {
        let t = tree(vec![
            (0, 1, vec![entry("日本語のフォルダ", 0, true), entry("b.bin", 500 * MB, false)]),
            (1, 3, vec![entry("データ.bin", 900 * MB, false)]),
        ]);
        let mut a = App::new(t, None, Config { reduced: true, dry_run: true, truecolor: true, aspect: 2.0 });
        a.resize(100, 30);
        let s = screen(&mut a, 100, 30);
        assert!(s.contains("日本語のフォルダ/"));
        // Each wide glyph takes two cells, so the dump is shorter in chars
        // than in cells but never wider than the screen.
        assert!(s.lines().all(|l| width(l) <= 100), "{s}");
    }
}
