//! Squarified treemap layout (Bruls, Huizing and van Wijk).

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RectF {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl RectF {
    pub fn lerp(self, to: RectF, t: f64) -> RectF {
        RectF {
            x: self.x + (to.x - self.x) * t,
            y: self.y + (to.y - self.y) * t,
            w: self.w + (to.w - self.w) * t,
            h: self.h + (to.h - self.h) * t,
        }
    }

    /// Maps this rectangle from the coordinate space of `src` into `dst`.
    pub fn map(self, src: RectF, dst: RectF) -> RectF {
        RectF {
            x: dst.x + (self.x - src.x) * dst.w / src.w,
            y: dst.y + (self.y - src.y) * dst.h / src.h,
            w: self.w * dst.w / src.w,
            h: self.h * dst.h / src.h,
        }
    }
}

/// Lays `values` (largest first, all positive) out inside the given
/// rectangle and returns one rectangle per value, in the same order.
pub fn squarify(values: &[f64], mut x: f64, mut y: f64, mut w: f64, mut h: f64) -> Vec<RectF> {
    let mut out = Vec::with_capacity(values.len());
    let total: f64 = values.iter().sum();
    if total <= 0.0 || !total.is_finite() || w <= 0.0 || h <= 0.0 {
        return out;
    }
    let k = w * h / total;
    let mut i = 0;
    while i < values.len() {
        let side = w.min(h).max(1e-9);
        let start = i;
        let (mut sum, mut mx, mut mn, mut worst) = (0.0f64, 0.0f64, f64::INFINITY, f64::INFINITY);
        while i < values.len() {
            let a = values[i] * k;
            let (s2, mx2, mn2) = (sum + a, mx.max(a), mn.min(a));
            let ratio = (side * side * mx2 / (s2 * s2)).max(s2 * s2 / (side * side * mn2));
            if i > start && ratio > worst {
                break;
            }
            sum = s2;
            mx = mx2;
            mn = mn2;
            worst = ratio;
            i += 1;
        }
        if w >= h {
            let col = sum / h;
            let mut yy = y;
            for v in &values[start..i] {
                let hh = v * k / col;
                out.push(RectF { x, y: yy, w: col, h: hh });
                yy += hh;
            }
            x += col;
            w -= col;
        } else {
            let row = sum / w;
            let mut xx = x;
            for v in &values[start..i] {
                let ww = v * k / row;
                out.push(RectF { x: xx, y, w: ww, h: row });
                xx += ww;
            }
            y += row;
            h -= row;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn areas_are_proportional_and_fill_the_space() {
        let vals = [50.0, 20.0, 15.0, 10.0, 4.0, 1.0];
        let rects = squarify(&vals, 2.0, 3.0, 80.0, 40.0);
        assert_eq!(rects.len(), vals.len());
        let total: f64 = vals.iter().sum();
        let mut area = 0.0;
        for (r, v) in rects.iter().zip(vals) {
            assert!((r.w * r.h - v / total * 3200.0).abs() < 1e-6);
            assert!(r.x >= 2.0 - 1e-9 && r.y >= 3.0 - 1e-9);
            assert!(r.x + r.w <= 82.0 + 1e-6 && r.y + r.h <= 43.0 + 1e-6);
            area += r.w * r.h;
        }
        assert!((area - 3200.0).abs() < 1e-6);
    }

    #[test]
    fn tiles_do_not_overlap() {
        let vals = [9.0, 7.0, 5.0, 3.0, 2.0, 2.0, 1.0];
        let rects = squarify(&vals, 0.0, 0.0, 30.0, 20.0);
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                let ox = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
                let oy = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
                assert!(ox <= 1e-6 || oy <= 1e-6, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn degenerate_input_gives_nothing() {
        assert!(squarify(&[], 0.0, 0.0, 10.0, 10.0).is_empty());
        assert!(squarify(&[1.0], 0.0, 0.0, 0.0, 10.0).is_empty());
        assert!(squarify(&[0.0, 0.0], 0.0, 0.0, 10.0, 10.0).is_empty());
    }

    #[test]
    fn mapping_between_spaces_round_trips() {
        let m = RectF { x: 10.0, y: 5.0, w: 100.0, h: 30.0 };
        let r0 = RectF { x: 40.0, y: 10.0, w: 20.0, h: 6.0 };
        let t = RectF { x: 35.0, y: 20.0, w: 50.0, h: 15.0 };
        let back = t.map(m, r0).map(r0, m);
        assert!((back.x - t.x).abs() < 1e-9 && (back.h - t.h).abs() < 1e-9);
        assert_eq!(m.map(m, r0), r0);
    }
}
