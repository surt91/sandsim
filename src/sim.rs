//! Sand height field: the ball ploughs, the sand slides (angle of repose).

use crate::params::SimParams;
use crate::util::{Rect, noise_field};

pub struct Sand {
    pub n: usize,
    pub res: f64,
    /// raw height field [mm], row 0 = top (+y), column 0 = left (-x)
    pub h: Vec<f32>,
    /// smoothed height field (edges rounded) – what is lit and displayed
    pub hs: Vec<f32>,
    pub mask: Vec<bool>,
    pub path: Vec<[f64; 2]>,
    pub n_clear: usize,
    /// index of the next path step
    pub k: usize,
    r: f64,
    push_w: f64,
    side: f64,
    tans: f64,
    tanr: f64,
    kernel: Vec<f32>,
    wbuf: Vec<f64>,
    sand_mm: f64,
}

impl Sand {
    pub fn new(p: &SimParams, path: Vec<[f64; 2]>, n_clear: usize) -> Self {
        let n = (2.0 * p.sand_mm / p.res) as usize + 1;
        let mut mask = vec![false; n * n];
        let noise = noise_field(n, 2.0, p.seed);
        let mut h = vec![0f32; n * n];
        for i in 0..n {
            for j in 0..n {
                let (x, y) = (j as f64 * p.res - p.sand_mm, i as f64 * p.res - p.sand_mm);
                let m = x.hypot(y) <= p.sand_mm;
                mask[i * n + j] = m;
                h[i * n + j] = if m { (p.h0 + 0.5 * noise[i * n + j]) as f32 } else { 0.0 };
            }
        }
        let sigma = p.smooth / p.res;
        let kernel = if sigma > 0.0 {
            let r = (4.0 * sigma + 0.5) as isize;
            let k: Vec<f64> = (-r..=r).map(|x| (-0.5 * (x as f64 / sigma).powi(2)).exp()).collect();
            let s: f64 = k.iter().sum();
            k.into_iter().map(|w| (w / s) as f32).collect()
        } else {
            vec![1.0]
        };
        let rr = ((p.ball_r + p.push_w) / p.res) as usize + 2;
        let mut s = Self {
            n,
            res: p.res,
            hs: h.clone(),
            h,
            mask,
            path,
            n_clear,
            k: 1,
            r: p.ball_r,
            push_w: p.push_w,
            side: p.side,
            tans: p.stat.to_radians().tan(),
            tanr: p.repose.to_radians().tan(),
            kernel,
            wbuf: vec![0.0; (2 * rr + 1) * (2 * rr + 1)],
            sand_mm: p.sand_mm,
        };
        s.smooth(Rect { i0: 0, i1: n, j0: 0, j1: n });
        s
    }

    pub fn done(&self) -> bool {
        self.k >= self.path.len()
    }

    /// Ball position in grid millimetres.
    pub fn ball(&self) -> [f64; 2] {
        self.path[self.k.min(self.path.len()) - 1]
    }

    /// Simulates up to `steps` path steps, returns the changed region of `hs`.
    pub fn advance(&mut self, steps: usize) -> Rect {
        let mut dirty = Rect::empty();
        let end = (self.k + steps).min(self.path.len());
        while self.k < end {
            dirty.union(self.plow(self.k));
            self.k += 1;
        }
        let m = self.kernel.len() / 2;
        let dirty = dirty.grow(m, self.n);
        self.smooth(dirty);
        dirty
    }

    fn plow(&mut self, k: usize) -> Rect {
        let (n, res, r) = (self.n, self.res, self.r);
        let [cx, cy] = self.path[k];
        let [px, py] = self.path[k - 1];
        let (mut dx, mut dy) = (cx - px, cy - py);
        let l = dx.hypot(dy);
        if l < 1e-9 {
            return Rect::empty();
        }
        dx /= l;
        dy /= l;
        let rr = ((r + self.push_w) / res) as isize + 2;
        let rw = rr + (3.0 / res) as isize;
        let (ci, cj) = ((cy / res + 0.5) as isize, (cx / res + 0.5) as isize);
        let clamp = |a: isize| a.clamp(0, n as isize) as usize;
        let (i0, i1, j0, j1) =
            (clamp(ci - rr), clamp(ci + rr + 1), clamp(cj - rr), clamp(cj + rr + 1));
        // 1) remove sand inside the ball volume
        let mut removed = 0.0;
        for i in i0..i1 {
            let ry = i as f64 * res - cy;
            for j in j0..j1 {
                let rx = j as f64 * res - cx;
                let r2 = rx * rx + ry * ry;
                if r2 < r * r {
                    let s = (r - (r * r - r2).sqrt()) as f32;
                    let h = &mut self.h[i * n + j];
                    if *h > s {
                        removed += (*h - s) as f64;
                        *h = s;
                    }
                }
            }
        }
        // 2) deposit it as a narrow bulge in front of and beside the ball
        if removed > 0.0 {
            let w2 = (2 * rr + 1) as usize;
            let mut wsum = 0.0;
            for i in i0..i1 {
                let ry = i as f64 * res - cy;
                for j in j0..j1 {
                    let mut w = 0.0;
                    if self.mask[i * n + j] {
                        let rx = j as f64 * res - cx;
                        let d = (rx * rx + ry * ry).sqrt();
                        if d >= r && d < r + self.push_w {
                            let c = (rx * dx + ry * dy) / d;
                            w = (c + self.side).max(0.0) * (1.0 - (d - r) / self.push_w);
                        }
                    }
                    let bi = (i as isize - ci + rr) as usize * w2 + (j as isize - cj + rr) as usize;
                    self.wbuf[bi] = w;
                    wsum += w;
                }
            }
            if wsum > 0.0 {
                let f = removed / wsum;
                for i in i0..i1 {
                    for j in j0..j1 {
                        let bi =
                            (i as isize - ci + rr) as usize * w2 + (j as isize - cj + rr) as usize;
                        self.h[i * n + j] += (f * self.wbuf[bi]) as f32;
                    }
                }
            }
        }
        // 3) let it slide
        let rect = Rect {
            i0: clamp(ci - rw),
            i1: clamp(ci + rw + 1),
            j0: clamp(cj - rw),
            j1: clamp(cj + rw + 1),
        };
        self.relax(rect, cx, cy, 3);
        rect
    }

    /// Angle of repose: slopes steeper than tans slide down to tanr (mass-conserving).
    /// Cells under the ball only take up sand to the ball surface.
    fn relax(&mut self, b: Rect, cx: f64, cy: f64, sweeps: usize) {
        // away from the rim all cells are sand, no need to check the mask
        let corner = |i: usize, j: usize| {
            let (x, y) = (j as f64 * self.res - self.sand_mm, i as f64 * self.res - self.sand_mm);
            (x * x + y * y).sqrt() < self.sand_mm - 2.0 * self.res
        };
        if corner(b.i0, b.j0) && corner(b.i0, b.j1) && corner(b.i1, b.j0) && corner(b.i1, b.j1) {
            self.relax_impl::<false>(b, cx, cy, sweeps);
        } else {
            self.relax_impl::<true>(b, cx, cy, sweeps);
        }
    }

    fn relax_impl<const MASK: bool>(&mut self, b: Rect, cx: f64, cy: f64, sweeps: usize) {
        let n = self.n;
        let dmax = (self.tans * self.res) as f32;
        let drest = (self.tanr * self.res) as f32;
        let (dmax2, drest2) = (dmax * 2f32.sqrt(), drest * 2f32.sqrt());
        let ball = Ball { cx, cy, r: self.r, res: self.res, n };
        let h = &mut self.h[..];
        let m = &self.mask[..];
        let ok = |p: usize| !MASK || m[p];
        for sw in 0..sweeps {
            let mut moved = 0.0;
            let rev = sw % 2 == 1; // alternate the sweep order, otherwise sand drifts one way
            for ii in b.i0..b.i1 {
                let i = if rev { b.i1 + b.i0 - 1 - ii } else { ii };
                for jj in b.j0..b.j1 {
                    let j = if rev { b.j1 + b.j0 - 1 - jj } else { jj };
                    let p = i * n + j;
                    if !ok(p) {
                        continue;
                    }
                    if j + 1 < b.j1 && ok(p + 1) {
                        moved += slide(h, p, p + 1, dmax, drest, &ball);
                    }
                    if i + 1 < b.i1 {
                        if ok(p + n) {
                            moved += slide(h, p, p + n, dmax, drest, &ball);
                        }
                        // diagonal neighbours too, otherwise pits and line ends come out square
                        if j + 1 < b.j1 && ok(p + n + 1) {
                            moved += slide(h, p, p + n + 1, dmax2, drest2, &ball);
                        }
                        if j > b.j0 && ok(p + n - 1) {
                            moved += slide(h, p, p + n - 1, dmax2, drest2, &ball);
                        }
                    }
                }
            }
            if moved < 1e-4 {
                break;
            }
        }
    }

    /// Recomputes the smoothed field in a region (rounds edges, mass-conserving).
    fn smooth(&mut self, b: Rect) {
        if b.is_empty() {
            return;
        }
        let n = self.n;
        let r = self.kernel.len() / 2;
        let k = &self.kernel;
        // horizontal pass on the rows the vertical pass needs
        let (ti0, ti1) = (b.i0.saturating_sub(r), (b.i1 + r).min(n));
        let w = b.j1 - b.j0;
        let mut tmp = vec![0f32; (ti1 - ti0) * w];
        for i in ti0..ti1 {
            for j in b.j0..b.j1 {
                let mut s = 0.0;
                for (d, kw) in k.iter().enumerate() {
                    let jj = (j + d).saturating_sub(r).min(n - 1);
                    s += kw * self.h[i * n + jj];
                }
                tmp[(i - ti0) * w + j - b.j0] = s;
            }
        }
        for i in b.i0..b.i1 {
            for j in b.j0..b.j1 {
                if !self.mask[i * n + j] {
                    self.hs[i * n + j] = 0.0;
                    continue;
                }
                let mut s = 0.0;
                for (d, kw) in k.iter().enumerate() {
                    let ii = (i + d).saturating_sub(r).clamp(ti0, ti1 - 1);
                    s += kw * tmp[(ii - ti0) * w + j - b.j0];
                }
                self.hs[i * n + j] = s;
            }
        }
    }
}

struct Ball {
    cx: f64,
    cy: f64,
    r: f64,
    res: f64,
    n: usize,
}

/// Moves sand between two neighbouring cells if the slope exceeds dmax.
#[inline(always)]
fn slide(h: &mut [f32], p: usize, q: usize, dmax: f32, drest: f32, ball: &Ball) -> f32 {
    let d = h[p] - h[q];
    let (hi, lo, d) = if d > dmax {
        (p, q, d)
    } else if d < -dmax {
        (q, p, -d)
    } else {
        return 0.0;
    };
    let mut amount = 0.5 * (d - drest);
    // limit space under the ball
    let rx = (lo % ball.n) as f64 * ball.res - ball.cx;
    let ry = (lo / ball.n) as f64 * ball.res - ball.cy;
    let r2 = rx * rx + ry * ry;
    if r2 < ball.r * ball.r {
        let cap = (ball.r - (ball.r * ball.r - r2).sqrt()) as f32 - h[lo];
        if cap <= 0.0 {
            return 0.0;
        }
        amount = amount.min(cap);
    }
    h[hi] -= amount;
    h[lo] += amount;
    amount
}
