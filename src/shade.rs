//! Lighting of the height field by the LED strip, incremental in tiles.
//!
//! Direct light (Lambert, cast shadows by horizon search) and ambient occlusion are
//! stored per pixel; colours are composed from them with a global normalisation
//! (95th percentile of direct light, median of total light), estimated from samples.

use crate::light::{Grid, Kernel};
use crate::params::{BG, LightParams, SAND_TINT, TONE, TONE_X};
use crate::pool::{MAX_OUTSTANDING, Pool, TileJob};
use crate::sim::Sand;
use crate::util::{Rect, noise_field};
use egui::{Color32, ColorImage};
use std::sync::atomic::{AtomicU32, Ordering};
use web_time::Instant;

pub const TILE: usize = 32;
const LUT_N: usize = 4096;
const LUT_MAX: f64 = 9.0;
const FINE: u8 = 0;
const ROUGH: u8 = 1;
const INIT: u8 = 2; // never lit
const DIRTY: u8 = 3;
/// tiles per job sent to a worker
const MIN_JOB: usize = 2;
const MAX_JOB: usize = 16;

static RUNS: AtomicU32 = AtomicU32::new(0);

pub struct Shader {
    pub l: LightParams,
    n: usize,
    res: f64,
    sand_mm: f64,
    tiles: usize,
    /// per tile: DIRTY, INIT, ROUGH (lit with few LEDs) or FINE
    state: Vec<u8>,
    /// per tile: bumped whenever its lighting becomes outdated
    generation: Vec<u32>,
    /// per tile: being lit by a web worker
    inflight: Vec<bool>,
    /// identifies this simulation run towards the web workers
    run: u32,
    /// version of the light parameters (for the web workers)
    version: u32,
    /// height field region changed since the workers were last updated
    hs_dirty: Rect,
    grid: Grid,
    kernel: Kernel,
    direct: Vec<f32>,
    ambient: Vec<f32>,
    albedo: Vec<f32>,
    samples: Vec<usize>,
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
    /// tiles lit in the last frame and the time it took on the main thread [ms]
    pub stats: (usize, f64),
    /// web workers in use
    pub workers: usize,
    /// tiles received from the workers, not composed yet
    received: Vec<usize>,
}

impl Shader {
    pub fn new(sand: &Sand, sand_mm: f64, l: LightParams) -> Self {
        let n = sand.n;
        let tiles = n.div_ceil(TILE);
        let samples = (0..n * n).filter(|&p| sand.mask[p]).step_by(37).collect();
        let mut s = Self {
            l: l.clone(),
            n,
            res: sand.res,
            sand_mm,
            tiles,
            state: vec![INIT; tiles * tiles],
            generation: vec![0; tiles * tiles],
            inflight: vec![false; tiles * tiles],
            run: RUNS.fetch_add(1, Ordering::Relaxed),
            version: 0,
            hs_dirty: Rect::empty(),
            grid: Grid::new(n, sand.res, sand_mm),
            kernel: Kernel::new(&l, sand.res),
            direct: vec![0.0; n * n],
            ambient: vec![0.0; n * n],
            albedo: Vec::new(),
            samples,
            lut: Vec::new(),
            norm: (1.0, 1.0),
            composed_norm: (0.0, 0.0),
            image: ColorImage::filled([n, n], Color32::from_rgb(9, 6, 6)),
            changed: Vec::new(),
            full_change: true,
            stats: (0, 0.0),
            last_full: None,
            sand_rgb: [0.6, 0.3, 0.2],
            workers: 0,
            received: Vec::new(),
        };
        s.setup();
        s
    }

    /// (Re)computes everything that depends on the light parameters.
    fn setup(&mut self) {
        let (n, res, l) = (self.n, self.res, &self.l);
        let gs = 0.125 / res; // grain size ~0.25 mm
        let a = noise_field(n, gs, 3);
        self.albedo = a.iter().map(|v| (1.0 + l.albedo_noise * v) as f32).collect();
        self.kernel = Kernel::new(l, res);
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
        self.version += 1;
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
        self.generation.iter_mut().for_each(|g| *g += 1);
    }

    /// Marks the region whose lighting depends on a height change in `r`: the changed
    /// tiles must be re-lit, the ones within shadow range only need refinement.
    pub fn mark(&mut self, r: Rect) {
        if r.is_empty() {
            return;
        }
        self.hs_dirty.union(r);
        let m = self.kernel.reach(self.res);
        for (r, level) in [(r.grow(m, self.n), ROUGH), (r.grow(2, self.n), DIRTY)] {
            for ti in r.i0 / TILE..=(r.i1 - 1) / TILE {
                for tj in r.j0 / TILE..=(r.j1 - 1) / TILE {
                    let t = ti * self.tiles + tj;
                    self.state[t] = self.state[t].max(level);
                    if level == ROUGH {
                        self.generation[t] += 1; // once per tile and mark
                    }
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

    /// Lights dirty tiles (closest to `focus` first): on the web workers if there are any,
    /// otherwise on the main thread until `budget_ms` is used up.
    /// While `live`, they are lit quickly with few LEDs; spare time and idle frames
    /// refine them with the full LED strip.
    pub fn work(
        &mut self,
        sand: &Sand,
        focus: [f64; 2],
        budget_ms: f64,
        live: bool,
        pool: Option<&mut Pool>,
    ) {
        let t0 = Instant::now();
        let (fi, fj) = (focus[1] / self.res / TILE as f64, focus[0] / self.res / TILE as f64);
        let dist = |t: usize| {
            let (ti, tj) = ((t / self.tiles) as f64 + 0.5, (t % self.tiles) as f64 + 0.5);
            (ti - fi).powi(2) + (tj - fj).powi(2)
        };
        // changed tiles first, then never lit ones, both closest to the ball first
        let mut todo: Vec<usize> =
            (0..self.state.len()).filter(|&t| self.state[t] >= INIT && !self.inflight[t]).collect();
        todo.sort_by(|&a, &b| self.state[b].cmp(&self.state[a]).then(dist(a).total_cmp(&dist(b))));
        let mut refine: Vec<usize> = (0..self.state.len())
            .filter(|&t| self.state[t] == ROUGH && !self.inflight[t])
            .collect();
        // while drawing, refine far from the ball first (near it, tiles get dirty again soon)
        refine.sort_by(|&a, &b| {
            if live { dist(b).total_cmp(&dist(a)) } else { dist(a).total_cmp(&dist(b)) }
        });
        let mut order =
            todo.into_iter().map(|t| (t, !live)).chain(refine.into_iter().map(|t| (t, true)));
        self.stats = (0, 0.0);
        let mut done = Vec::new();
        match pool {
            Some(pool) => {
                self.workers = pool.size();
                self.sync(pool, sand);
                self.receive(Some(pool));
                // spread the changed tiles evenly over the workers, so that they come back
                // together; then fill up with refinement work
                let urgent = (0..self.state.len())
                    .filter(|&t| self.state[t] == DIRTY && !self.inflight[t])
                    .count();
                let chunk = urgent.div_ceil(pool.size()).clamp(MIN_JOB, MAX_JOB);
                'dispatch: for _ in 0..MAX_OUTSTANDING {
                    for k in pool.free() {
                        let jobs: Vec<TileJob> = order
                            .by_ref()
                            .take(chunk)
                            .map(|(t, fine)| {
                                self.inflight[t] = true;
                                TileJob {
                                    tile: t,
                                    rect: self.tile_rect(t),
                                    fine,
                                    generation: self.generation[t],
                                }
                            })
                            .collect();
                        if jobs.is_empty() {
                            break 'dispatch;
                        }
                        pool.send(k, self.run, &jobs);
                    }
                }
            }
            None => {
                let mut direct = vec![0.0; TILE * TILE];
                let mut ambient = vec![0.0; TILE * TILE];
                for (t, fine) in order {
                    if t0.elapsed().as_secs_f64() * 1e3 > budget_ms {
                        break;
                    }
                    self.state[t] = if fine { FINE } else { ROUGH };
                    let t1 = Instant::now();
                    let r = self.tile_rect(t);
                    let w = r.j1 - r.j0;
                    self.kernel.light(&self.grid, &sand.hs, r, fine, &mut direct, &mut ambient);
                    for i in r.i0..r.i1 {
                        let (p, q) = (i * self.n + r.j0, (i - r.i0) * w);
                        self.direct[p..p + w].copy_from_slice(&direct[q..q + w]);
                        self.ambient[p..p + w].copy_from_slice(&ambient[q..q + w]);
                    }
                    self.stats.0 += 1;
                    self.stats.1 += t1.elapsed().as_secs_f64() * 1e3;
                    done.push(t);
                }
            }
        }
        self.stats.0 += self.received.len();
        done.append(&mut self.received);
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

    /// Applies the tiles the workers have finished (they are shown with the next `work`).
    pub fn receive(&mut self, pool: Option<&mut Pool>) {
        let Some(pool) = pool else { return };
        for r in pool.take() {
            if r.run != self.run {
                continue;
            }
            let t = r.tile;
            self.inflight[t] = false;
            let w = r.rect.j1 - r.rect.j0;
            for i in r.rect.i0..r.rect.i1 {
                let (p, q) = (i * self.n + r.rect.j0, (i - r.rect.i0) * w);
                self.direct[p..p + w].copy_from_slice(&r.direct[q..q + w]);
                self.ambient[p..p + w].copy_from_slice(&r.ambient[q..q + w]);
            }
            // a result for an outdated tile is still shown, but the tile stays due
            if r.generation == self.generation[t] {
                self.state[t] = if r.fine { FINE } else { ROUGH };
            }
            self.received.push(t);
        }
    }

    /// Brings the workers up to date: run, light parameters, height field.
    fn sync(&mut self, pool: &mut Pool, sand: &Sand) {
        let (run, version) = pool.synced();
        if run != self.run {
            let (n, res, mm) = (self.n, self.res, self.sand_mm);
            pool.start_run(self.run, self.version, n, res, mm, &sand.hs, &self.l);
            self.hs_dirty = Rect::empty();
        } else if version != self.version {
            pool.set_params(self.version, &self.l);
        }
        if !self.hs_dirty.is_empty() {
            pool.update(self.hs_dirty, &sand.hs, self.n);
            self.hs_dirty = Rect::empty();
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
