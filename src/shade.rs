//! Lighting of the height field by the LED strip, incremental in tiles.
//!
//! Direct light (Lambert, cast shadows by horizon search) and ambient occlusion are
//! stored per pixel; colours are composed from them with a global normalisation
//! (95th percentile of direct light, median of total light), estimated from samples.

use crate::params::{BG, LightParams, SAND_TINT, TONE, TONE_X};
use crate::sim::Sand;
use crate::util::{Rect, noise_field};
use egui::{Color32, ColorImage};
use web_time::Instant;

pub const TILE: usize = 32;
const LUT_N: usize = 4096;
const LUT_MAX: f64 = 9.0;
const FINE: u8 = 0;
const ROUGH: u8 = 1;
const INIT: u8 = 2; // never lit
const DIRTY: u8 = 3;

pub struct Shader {
    pub l: LightParams,
    n: usize,
    res: f64,
    sand_mm: f64,
    tiles: usize,
    /// per tile: DIRTY, INIT, ROUGH (lit with few LEDs) or FINE
    state: Vec<u8>,
    direct: Vec<f32>,
    ambient: Vec<f32>,
    grain: Vec<f64>,
    albedo: Vec<f32>,
    samples: Vec<usize>,
    leds_fine: Vec<[f64; 2]>,
    leds_rough: Vec<[f64; 2]>,
    led_steps: Vec<(f32, f32)>,
    ao_rays: Vec<Vec<(isize, isize, f32)>>,
    lut: Vec<[f32; 3]>,
    norm: (f64, f64),
    composed_norm: (f64, f64),
    pub image: ColorImage,
    /// tiles whose colours changed since the last texture upload
    pub changed: Vec<usize>,
    pub full_change: bool,
    last_full: Option<Instant>,
    /// typical sand colour, 0..1
    pub sand_rgb: [f64; 3],
    /// tiles lit in the last frame and the time it took [ms]
    pub stats: (usize, f64),
}

impl Shader {
    pub fn new(sand: &Sand, sand_mm: f64, l: LightParams) -> Self {
        let n = sand.n;
        let tiles = n.div_ceil(TILE);
        let samples = (0..n * n).filter(|&p| sand.mask[p]).step_by(37).collect();
        let mut s = Self {
            l,
            n,
            res: sand.res,
            sand_mm,
            tiles,
            state: vec![INIT; tiles * tiles],
            direct: vec![0.0; n * n],
            ambient: vec![0.0; n * n],
            grain: Vec::new(),
            albedo: Vec::new(),
            samples,
            leds_fine: Vec::new(),
            leds_rough: Vec::new(),
            led_steps: Vec::new(),
            ao_rays: Vec::new(),
            lut: Vec::new(),
            norm: (1.0, 1.0),
            composed_norm: (0.0, 0.0),
            image: ColorImage::filled([n, n], Color32::from_rgb(9, 6, 6)),
            changed: Vec::new(),
            full_change: true,
            stats: (0, 0.0),
            last_full: None,
            sand_rgb: [0.6, 0.3, 0.2],
        };
        s.setup();
        s
    }

    /// (Re)computes everything that depends on the light parameters.
    fn setup(&mut self) {
        let (n, res, l) = (self.n, self.res, &self.l);
        let gs = 0.125 / res; // grain size ~0.25 mm
        self.grain = noise_field(n, gs, 2);
        let a = noise_field(n, gs, 3);
        self.albedo = a.iter().map(|v| (1.0 + l.albedo_noise * v) as f32).collect();
        let ring = |m: usize| -> Vec<[f64; 2]> {
            (0..m)
                .map(|k| {
                    let phi = std::f64::consts::TAU * k as f64 / m as f64;
                    [phi.cos() * l.r_led, phi.sin() * l.r_led]
                })
                .collect()
        };
        self.leds_fine = ring(l.n_led);
        self.leds_rough = ring(l.n_led_live.min(l.n_led));
        self.led_steps = steps(l.shadow_mm / res, 0.08)
            .into_iter()
            .map(|s| (s as f32, (1.0 / (s * res)) as f32))
            .collect();
        self.ao_rays = (0..12)
            .map(|k| {
                let a = std::f64::consts::TAU * (k as f64 + 0.5) / 12.0;
                steps(l.ao_mm / res, 0.1)
                    .into_iter()
                    .map(|s| {
                        (
                            (a.sin() * s).round() as isize,
                            (a.cos() * s).round() as isize,
                            (1.0 / (s * res)) as f32,
                        )
                    })
                    .collect()
            })
            .collect();
        let tone: Vec<[f64; 3]> = match l.led_color {
            None => TONE.to_vec(),
            Some(led) => {
                // same brightness curve (brightest channel of the calibrated colours),
                // hue from the sand lit by the chosen LED colour
                let c = [0, 1, 2].map(|k| SAND_TINT[k] * led[k] as f64 / 255.0);
                let m = c.iter().cloned().fold(1e-6, f64::max);
                TONE.iter()
                    .map(|t| {
                        let v = t.iter().cloned().fold(0.0, f64::max);
                        c.map(|ck| v * ck / m)
                    })
                    .collect()
            }
        };
        self.lut = (0..LUT_N)
            .map(|k| {
                let x = k as f64 / (LUT_N - 1) as f64 * LUT_MAX;
                let mut c = [0f32; 3];
                for (ch, cc) in c.iter_mut().enumerate() {
                    *cc = (interp(x, &TONE_X, &tone, ch) / 255.0) as f32;
                }
                c
            })
            .collect();
    }

    pub fn set_params(&mut self, l: LightParams) {
        if l == self.l {
            return;
        }
        // exposure and colours only change the colouring, not the light
        let colour_only = LightParams {
            exposure: self.l.exposure,
            led_color: self.l.led_color,
            albedo_noise: self.l.albedo_noise,
            ..l.clone()
        } == self.l;
        self.l = l;
        self.setup();
        if colour_only {
            self.composed_norm = (0.0, 0.0); // forces a full re-colouring
            self.last_full = None;
            self.full_change = true;
        } else {
            self.mark_all();
        }
    }

    pub fn mark_all(&mut self) {
        self.state.iter_mut().for_each(|d| *d = DIRTY);
    }

    /// Marks the region whose lighting depends on a height change in `r`: the changed
    /// tiles must be re-lit, the ones within shadow range only need refinement.
    pub fn mark(&mut self, r: Rect) {
        if r.is_empty() {
            return;
        }
        let m = (self.l.shadow_mm.max(self.l.ao_mm) / self.res) as usize + 2;
        for (r, level) in [(r.grow(m, self.n), ROUGH), (r.grow(2, self.n), DIRTY)] {
            for ti in r.i0 / TILE..=(r.i1 - 1) / TILE {
                for tj in r.j0 / TILE..=(r.j1 - 1) / TILE {
                    let s = &mut self.state[ti * self.tiles + tj];
                    *s = (*s).max(level);
                }
            }
        }
    }

    /// Tiles that still show outdated sand (or nothing yet).
    pub fn pending(&self) -> usize {
        self.state.iter().filter(|&&d| d >= INIT).count()
    }

    /// Tiles changed by the ball that are not lit yet.
    pub fn dirty(&self) -> usize {
        self.state.iter().filter(|&&d| d == DIRTY).count()
    }

    /// Tiles that are only lit with the reduced LED set.
    pub fn rough(&self) -> usize {
        self.state.iter().filter(|&&d| d == ROUGH).count()
    }

    pub fn total_tiles(&self) -> usize {
        self.state.len()
    }

    /// Lights dirty tiles (closest to `focus` first) until `budget_ms` is used up.
    /// While `live`, they are lit quickly with few LEDs; spare time and idle frames
    /// refine them with the full LED strip.
    pub fn work(&mut self, sand: &Sand, focus: [f64; 2], budget_ms: f64, live: bool) {
        let t0 = Instant::now();
        let (fi, fj) = (focus[1] / self.res / TILE as f64, focus[0] / self.res / TILE as f64);
        let dist = |t: usize| {
            let (ti, tj) = ((t / self.tiles) as f64 + 0.5, (t % self.tiles) as f64 + 0.5);
            (ti - fi).powi(2) + (tj - fj).powi(2)
        };
        // changed tiles first, then never lit ones, both closest to the ball first
        let mut todo: Vec<usize> =
            (0..self.state.len()).filter(|&t| self.state[t] >= INIT).collect();
        todo.sort_by(|&a, &b| self.state[b].cmp(&self.state[a]).then(dist(a).total_cmp(&dist(b))));
        let mut refine: Vec<usize> =
            (0..self.state.len()).filter(|&t| self.state[t] == ROUGH).collect();
        // while drawing, refine far from the ball first (near it, tiles get dirty again soon)
        refine.sort_by(|&a, &b| {
            if live { dist(b).total_cmp(&dist(a)) } else { dist(a).total_cmp(&dist(b)) }
        });
        let mut done = Vec::new();
        self.stats = (0, 0.0);
        for (t, fine) in
            todo.into_iter().map(|t| (t, !live)).chain(refine.into_iter().map(|t| (t, true)))
        {
            if t0.elapsed().as_secs_f64() * 1e3 > budget_ms {
                break;
            }
            self.state[t] = if fine { FINE } else { ROUGH };
            let t1 = Instant::now();
            self.light_tile(sand, t, fine);
            self.stats.0 += 1;
            self.stats.1 += t1.elapsed().as_secs_f64() * 1e3;
            done.push(t);
        }
        if done.is_empty() && !self.full_change {
            return;
        }
        self.update_norm();
        let (a, b) = (self.norm, self.composed_norm);
        let drift = ((a.0 / b.0) - 1.0).abs() > 0.004 || ((a.1 / b.1) - 1.0).abs() > 0.004;
        // re-colouring everything (and uploading it) is expensive: at most once a second
        // while there is work left, new tiles use the previous normalisation meanwhile
        let idle = self.state.iter().all(|&s| s == FINE);
        let due = self.last_full.is_none_or(|t| t.elapsed().as_secs_f64() > 1.0);
        if drift && (idle || due) {
            self.composed_norm = self.norm;
            self.compose(Rect { i0: 0, i1: self.n, j0: 0, j1: self.n }, sand);
            self.last_full = Some(Instant::now());
            self.full_change = true;
            self.sand_rgb = self.sand_color();
        } else {
            for &t in &done {
                self.compose(self.tile_rect(t), sand);
            }
            self.changed.extend(done);
        }
    }

    pub fn tile_rect(&self, t: usize) -> Rect {
        let (ti, tj) = (t / self.tiles, t % self.tiles);
        Rect {
            i0: ti * TILE,
            i1: ((ti + 1) * TILE).min(self.n),
            j0: tj * TILE,
            j1: ((tj + 1) * TILE).min(self.n),
        }
    }

    fn light_tile(&mut self, sand: &Sand, t: usize, fine: bool) {
        let r = self.tile_rect(t);
        let (n, res, l) = (self.n, self.res, &self.l);
        let g2 = 2.0 * l.grain;
        let hg = |i: usize, j: usize| sand.hs[i * n + j] as f64 + g2 * self.grain[i * n + j];
        let r0 = self.sand_mm;
        let a0 = l.alpha0.to_radians();
        let soft = l.soft.to_radians();
        let ts = (0.5 * soft).tan();
        let nf = n as f32;
        let hs = &sand.hs[..];
        let leds = if fine { &self.leds_fine } else { &self.leds_rough };
        // highest sand within reach of the tile: once even that can't change the result,
        // the horizon search along a ray stops early
        let reach = r.grow((l.shadow_mm.max(l.ao_mm) / res) as usize + 2, n);
        let mut hmax = f32::MIN;
        for i in reach.i0..reach.i1 {
            hmax = hs[i * n + reach.j0..i * n + reach.j1].iter().fold(hmax, |a, &b| a.max(b));
        }
        let wl = 48.0 / leds.len() as f64;
        for i in r.i0..r.i1 {
            let y = r0 - i as f64 * res;
            for j in r.j0..r.j1 {
                let p = i * n + j;
                if !sand.mask[p] {
                    continue;
                }
                let x = j as f64 * res - r0;
                // normal from the grainy height field (grain only in the normals, not in shadows)
                let (ja, jb) = (j.saturating_sub(1), (j + 1).min(n - 1));
                let (ia, ib) = (i.saturating_sub(1), (i + 1).min(n - 1));
                let gx = (hg(i, jb) - hg(i, ja)) / ((jb - ja) as f64 * res);
                let gy = (hg(ib, j) - hg(ia, j)) / ((ib - ia) as f64 * res);
                let nz = 1.0 / (1.0 + gx * gx + gy * gy).sqrt();
                let (nx, ny) = (-gx * nz, gy * nz); // rows run towards -y
                let h32 = hs[p];
                let h = h32 as f64;
                // direct light from the LED strip with cast shadows (horizon search)
                let mut acc = 0.0;
                for led in leds {
                    let (mut lx, mut ly) = (led[0] - x, led[1] - y);
                    let d = (lx * lx + ly * ly).sqrt();
                    let dz = l.z_led - h;
                    lx /= d;
                    ly /= d;
                    let q = dz / d;
                    let c = (nx * lx + ny * ly + nz * q) / (1.0 + q * q).sqrt();
                    if c <= 0.0 {
                        continue;
                    }
                    let el = q.atan();
                    let em = (-(el / a0).powi(2) - l.falloff * d.ln()).exp();
                    // fully lit below t_lo, fully shadowed above t_hi (tangents of el ∓ soft/2)
                    let t_lo = (q - ts) / (1.0 + q * ts);
                    let t_hi = if q * ts < 1.0 { (q + ts) / (1.0 - q * ts) } else { f64::MAX };
                    let (t_hi32, lx32, ly32) = (t_hi as f32, lx as f32, ly as f32);
                    let (fi, fj) = (i as f32 + 0.5, j as f32 + 0.5);
                    let mut tmax = -1.0f32;
                    let mut floor = t_lo as f32; // below this, the LED is fully visible
                    let above = hmax - h32;
                    for &(s, inv) in &self.led_steps {
                        if above * inv <= floor {
                            break;
                        }
                        let ii = (fi - ly32 * s).floor();
                        let jj = (fj + lx32 * s).floor();
                        if ii < 0.0 || jj < 0.0 || ii >= nf || jj >= nf {
                            break;
                        }
                        let tt = (hs[ii as usize * n + jj as usize] - h32) * inv;
                        if tt > tmax {
                            tmax = tt;
                            floor = floor.max(tmax);
                            if tmax >= t_hi32 {
                                break;
                            }
                        }
                    }
                    let tmax = tmax as f64;
                    let vis = if tmax >= t_hi {
                        continue;
                    } else if tmax <= t_lo {
                        1.0
                    } else {
                        ((el - tmax.atan()) / soft + 0.5).clamp(0.0, 1.0)
                    };
                    acc += c * em * vis;
                }
                self.direct[p] = (acc * wl) as f32;
                // sky visibility (cosine-weighted) from horizon angles in 12 directions
                let mut v = 0.0;
                for ray in &self.ao_rays {
                    let mut tmax = 0.0f32;
                    for &(di, dj, inv) in ray {
                        if (hmax - h32) * inv <= tmax {
                            break;
                        }
                        let (ii, jj) = (i as isize + di, j as isize + dj);
                        if ii < 0 || jj < 0 || ii >= n as isize || jj >= n as isize {
                            break;
                        }
                        tmax = tmax.max((hs[ii as usize * n + jj as usize] - h32) * inv);
                    }
                    let tmax = tmax as f64;
                    v += 1.0 / (1.0 + tmax * tmax); // cos²(horizon angle)
                }
                v /= self.ao_rays.len() as f64;
                self.ambient[p] = (l.ambient * v.powf(l.ao) * nz) as f32;
            }
        }
    }

    fn update_norm(&mut self) {
        let mut d: Vec<f32> = self.samples.iter().map(|&p| self.direct[p]).collect();
        let p95 = percentile(&mut d, 0.95).max(1e-12) as f64;
        let mut tot: Vec<f32> = self
            .samples
            .iter()
            .map(|&p| (self.direct[p] as f64 / p95 + self.ambient[p] as f64).max(0.0) as f32)
            .collect();
        let p50 = percentile(&mut tot, 0.5).max(1e-12) as f64;
        self.norm = (p95, p50);
    }

    fn compose(&mut self, r: Rect, sand: &Sand) {
        let n = self.n;
        let (p95, p50) = self.composed_norm;
        let k = self.l.exposure / p50;
        let bg =
            Color32::from_rgb((BG[0] * 255.0) as u8, (BG[1] * 255.0) as u8, (BG[2] * 255.0) as u8);
        for i in r.i0..r.i1 {
            let y = self.sand_mm - i as f64 * self.res;
            for j in r.j0..r.j1 {
                let p = i * n + j;
                if !sand.mask[p] {
                    self.image.pixels[p] = bg;
                    continue;
                }
                let x = j as f64 * self.res - self.sand_mm;
                let v = ((self.direct[p] as f64 / p95 + self.ambient[p] as f64).max(0.0) * k)
                    .min(LUT_MAX);
                let c = self.lut[(v / LUT_MAX * (LUT_N - 1) as f64) as usize];
                // grain colour after the tone curve, so that shadows do not get speckled;
                // thin shadow line at the wall
                let f = self.albedo[p]
                    * ((self.sand_mm - (x * x + y * y).sqrt()) / 1.5).clamp(0.4, 1.0) as f32;
                let u = |v: f32| ((v * f).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                self.image.pixels[p] = Color32::from_rgb(u(c[0]), u(c[1]), u(c[2]));
            }
        }
    }

    /// Typical bright sand colour (for the reflections in the ball), 0..1.
    pub fn sand_color(&self) -> [f64; 3] {
        let mut ch: [Vec<f32>; 3] = Default::default();
        for &p in &self.samples {
            let c = self.image.pixels[p];
            if c.r() as u32 + c.g() as u32 + c.b() as u32 > 76 {
                ch[0].push(c.r() as f32);
                ch[1].push(c.g() as f32);
                ch[2].push(c.b() as f32);
            }
        }
        let mut out = [0.6, 0.3, 0.2];
        for k in 0..3 {
            if !ch[k].is_empty() {
                out[k] = percentile(&mut ch[k], 0.7) as f64 / 255.0;
            }
        }
        out
    }
}

/// Geometrically growing march distances 1, 2+g, … below `max` (in pixels).
fn steps(max: f64, g: f64) -> Vec<f64> {
    let mut v = Vec::new();
    let mut s = 1.0;
    while s < max {
        v.push(s);
        s += 1.0 + s * g;
    }
    v
}

fn interp(x: f64, xs: &[f64], ys: &[[f64; 3]], ch: usize) -> f64 {
    if x <= xs[0] {
        return ys[0][ch];
    }
    for k in 1..xs.len() {
        if x <= xs[k] {
            let f = (x - xs[k - 1]) / (xs[k] - xs[k - 1]);
            return ys[k - 1][ch] + f * (ys[k][ch] - ys[k - 1][ch]);
        }
    }
    ys[ys.len() - 1][ch]
}

fn percentile(v: &mut [f32], q: f64) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let k = ((v.len() - 1) as f64 * q).round() as usize;
    *v.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1
}
