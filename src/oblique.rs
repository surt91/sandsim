//! Perspective oblique view: rays traced against the height field, row by row.

use crate::params::{BG, Camera};
use crate::sim::Sand;
use egui::{Color32, ColorImage};
use web_time::Instant;

pub struct Oblique {
    pub image: ColorImage,
    pub row: usize,
    c: [f64; 3],
    rm: [[f64; 3]; 3],
    f: f64,
    ball: [f64; 4],
    sand: [f64; 3],
    hm: f64,
    zmin: f64,
    zmax: f64,
}

impl Oblique {
    pub fn new(cam: &Camera, sand: &Sand, sand_mm: f64, ball_r: f64, sand_col: [f64; 3]) -> Self {
        let (w, h) = (cam.width, cam.height);
        let f = w as f64 / 2.0 / (cam.hfov.to_radians() / 2.0).tan();
        // rotate the camera around the table centre
        let (sa, ca) = cam.rot.to_radians().sin_cos();
        let rot = |x: f64, y: f64| [ca * x - sa * y, sa * x + ca * y];
        let [px, py] = rot(cam.pos[0], cam.pos[1]);
        let c = [px, py, cam.pos[2]];
        let [tx, ty] = rot(cam.target[0], cam.target[1]);
        let fw = norm([tx - c[0], ty - c[1], -c[2]]);
        let rt = norm(cross(fw, [0.0, 0.0, 1.0]));
        let dn = cross(fw, rt);
        let (s, co) = cam.roll.to_radians().sin_cos();
        let rm = [
            [0, 1, 2].map(|k| co * rt[k] + s * dn[k]), // camera x (right)
            [0, 1, 2].map(|k| co * dn[k] - s * rt[k]), // y (down)
            fw,                                        // viewing direction
        ];
        let hm = sand.hs.iter().map(|&v| v as f64).sum::<f64>() / sand.hs.len() as f64;
        let [bx, by] = sand.ball();
        let ball = [bx - sand_mm, sand_mm - by, ball_r - hm, ball_r];
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for v in sand.hs.iter().map(|&v| v as f64) {
            lo = lo.min(v);
            hi = hi.max(v);
        }
        Self {
            image: ColorImage::filled([w, h], Color32::from_rgb(9, 6, 6)),
            row: 0,
            c,
            rm,
            f,
            ball,
            sand: sand_col,
            hm,
            zmin: lo - hm - 0.01,
            zmax: (hi - hm).max(ball[2] + ball[3]) + 0.01,
        }
    }

    pub fn done(&self) -> bool {
        self.row >= self.image.size[1]
    }

    pub fn work(&mut self, sand: &Sand, tex: &ColorImage, budget_ms: f64) {
        let t0 = Instant::now();
        while !self.done() && t0.elapsed().as_secs_f64() * 1e3 < budget_ms {
            let v = self.row;
            for u in 0..self.image.size[0] {
                let c = self.trace(sand, tex, u, v);
                let px = |x: f64| (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                self.image.pixels[v * self.image.size[0] + u] =
                    Color32::from_rgb(px(c[0]), px(c[1]), px(c[2]));
            }
            self.row += 1;
        }
    }

    fn trace(&self, sand: &Sand, tex: &ColorImage, u: usize, v: usize) -> [f64; 3] {
        let (n, res) = (sand.n, sand.res);
        let r0 = (n - 1) as f64 * res / 2.0;
        let [w, hh] = self.image.size;
        let (c, rm, ball) = (self.c, self.rm, self.ball);
        let (dc0, dc1) =
            ((u as f64 - w as f64 / 2.0) / self.f, (v as f64 - hh as f64 / 2.0) / self.f);
        let d = norm([0, 1, 2].map(|k| rm[0][k] * dc0 + rm[1][k] * dc1 + rm[2][k]));
        let [dx, dy, dz] = d;
        if dz >= 0.0 {
            return BG;
        }
        // ball (ray-sphere intersection)
        let o = [c[0] - ball[0], c[1] - ball[1], c[2] - ball[2]];
        let b = o[0] * dx + o[1] * dy + o[2] * dz;
        let disc = b * b - (o[0] * o[0] + o[1] * o[1] + o[2] * o[2] - ball[3] * ball[3]);
        let tb = if disc > 0.0 { -b - disc.sqrt() } else { 1e30 };
        let mut t = (self.zmax - c[2]) / dz;
        let (mut x, mut y, mut z) = (c[0] + t * dx, c[1] + t * dy, self.zmax);
        let st = 0.5 * res / dx.hypot(dy).max(1e-3);
        let mut hit = false;
        for _ in 0..4000 {
            if z < self.zmin || t > tb {
                break;
            }
            let (fi, fj) = ((r0 - y) / res, (x + r0) / res);
            if fi >= 0.0 && fj >= 0.0 {
                let (i, j) = (fi as usize, fj as usize);
                if i + 1 < n && j + 1 < n {
                    let (a, cc) = (fi - i as f64, fj - j as f64);
                    let hs = |p: usize| sand.hs[p] as f64;
                    let hz = hs(i * n + j) * (1.0 - a) * (1.0 - cc)
                        + hs((i + 1) * n + j) * a * (1.0 - cc)
                        + hs(i * n + j + 1) * (1.0 - a) * cc
                        + hs((i + 1) * n + j + 1) * a * cc
                        - self.hm;
                    if z <= hz && x * x + y * y <= r0 * r0 {
                        hit = true;
                        break;
                    }
                }
            }
            x += st * dx;
            y += st * dy;
            z += st * dz;
            t += st;
        }
        if t > tb && tb < 1e29 {
            // steel ball: reflection of sand (below) and dark surroundings
            let p = [o[0] + tb * dx, o[1] + tb * dy, o[2] + tb * dz];
            let nzz = p[2] / ball[3];
            let k = dx * p[0] + dy * p[1] + dz * p[2];
            let rz = dz - 2.0 * k / (ball[3] * ball[3]) * p[2]; // reflection direction z
            let w = (-rz * 4.0 + 0.6).clamp(0.0, 1.0);
            self.sand.map(|s| s * (0.15 + 0.85 * w) + 0.03 + 0.6 * nzz.max(0.0).powi(40))
        } else if hit {
            // bilinear texture lookup
            let (fi, fj) = (((r0 - y) / res).max(0.0), ((x + r0) / res).max(0.0));
            let (i, j) = ((fi as usize).min(n - 2), (fj as usize).min(n - 2));
            let (a, b) = (fi - i as f64, fj - j as f64);
            let px = |i: usize, j: usize| {
                let c = tex.pixels[i * n + j];
                [c.r(), c.g(), c.b()].map(|v| v as f64 / 255.0)
            };
            let (p00, p01, p10, p11) = (px(i, j), px(i, j + 1), px(i + 1, j), px(i + 1, j + 1));
            [0, 1, 2].map(|k| {
                (p00[k] * (1.0 - b) + p01[k] * b) * (1.0 - a)
                    + (p10[k] * (1.0 - b) + p11[k] * b) * a
            })
        } else {
            BG
        }
    }
}

fn norm(a: [f64; 3]) -> [f64; 3] {
    let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    a.map(|v| v / l)
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
