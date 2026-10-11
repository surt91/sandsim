//! Lighting kernel: direct light from the LED strip (Lambert, cast shadows by horizon
//! search) and ambient occlusion for one tile. Shared by the main thread and the web
//! workers, so it only depends on plain data.

use crate::params::LightParams;
use crate::util::{Rect, noise_field};

/// Table geometry, fixed for one simulation run.
pub struct Grid {
    pub n: usize,
    pub res: f64,
    pub sand_mm: f64,
    pub mask: Vec<bool>,
    /// height noise for the normals (grain), unit amplitude
    pub grain: Vec<f32>,
}

impl Grid {
    pub fn new(n: usize, res: f64, sand_mm: f64) -> Self {
        let gs = 0.125 / res; // grain size ~0.25 mm
        Self {
            n,
            res,
            sand_mm,
            mask: table_mask(n, res, sand_mm),
            grain: noise_field(n, gs, 2).into_iter().map(|v| v as f32).collect(),
        }
    }
}

/// Cells that belong to the sand surface.
pub fn table_mask(n: usize, res: f64, sand_mm: f64) -> Vec<bool> {
    let mut mask = vec![false; n * n];
    for i in 0..n {
        for j in 0..n {
            let (x, y) = (j as f64 * res - sand_mm, i as f64 * res - sand_mm);
            mask[i * n + j] = x.hypot(y) <= sand_mm;
        }
    }
    mask
}

/// Everything derived from the light parameters.
pub struct Kernel {
    pub l: LightParams,
    leds_fine: Vec<[f64; 2]>,
    leds_rough: Vec<[f64; 2]>,
    led_steps: Vec<(f32, f32)>,
    ao_rays: Vec<Vec<(isize, isize, f32)>>,
}

impl Kernel {
    pub fn new(l: &LightParams, res: f64) -> Self {
        let ring = |m: usize| -> Vec<[f64; 2]> {
            (0..m)
                .map(|k| {
                    let phi = std::f64::consts::TAU * k as f64 / m as f64;
                    [phi.cos() * l.r_led, phi.sin() * l.r_led]
                })
                .collect()
        };
        let led_steps = steps(l.shadow_mm / res, 0.08)
            .into_iter()
            .map(|s| (s as f32, (1.0 / (s * res)) as f32))
            .collect();
        let ao_rays = (0..12)
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
        Self {
            l: l.clone(),
            leds_fine: ring(l.n_led),
            leds_rough: ring(l.n_led_live.min(l.n_led)),
            led_steps,
            ao_rays,
        }
    }

    /// Distance [px] up to which a height change affects the lighting.
    pub fn reach(&self, res: f64) -> usize {
        (self.l.shadow_mm.max(self.l.ao_mm) / res) as usize + 2
    }

    /// Lights the tile `r` of the height field `hs`; writes direct light and ambient light
    /// into the tile-sized buffers (row-major, width r.j1 - r.j0).
    pub fn light(
        &self,
        g: &Grid,
        hs: &[f32],
        r: Rect,
        fine: bool,
        direct: &mut [f32],
        ambient: &mut [f32],
    ) {
        let (n, res, l) = (g.n, g.res, &self.l);
        let g2 = (2.0 * l.grain) as f32;
        let hg = |i: usize, j: usize| (hs[i * n + j] + g2 * g.grain[i * n + j]) as f64;
        let r0 = g.sand_mm;
        let a0 = l.alpha0.to_radians();
        let soft = l.soft.to_radians();
        let ts = (0.5 * soft).tan();
        let nf = n as f32;
        let leds = if fine { &self.leds_fine } else { &self.leds_rough };
        // highest sand within reach of the tile: once even that can't change the result,
        // the horizon search along a ray stops early
        let reach = r.grow(self.reach(res), n);
        let mut hmax = f32::MIN;
        for i in reach.i0..reach.i1 {
            hmax = hs[i * n + reach.j0..i * n + reach.j1].iter().fold(hmax, |a, &b| a.max(b));
        }
        let wl = 48.0 / leds.len() as f64;
        let w = r.j1 - r.j0;
        for i in r.i0..r.i1 {
            let y = r0 - i as f64 * res;
            for j in r.j0..r.j1 {
                let p = i * n + j;
                let o = (i - r.i0) * w + j - r.j0;
                if !g.mask[p] {
                    direct[o] = 0.0;
                    ambient[o] = 0.0;
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
                direct[o] = (acc * wl) as f32;
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
                ambient[o] = (l.ambient * v.powf(l.ao) * nz) as f32;
            }
        }
    }
}

/// The light parameters the kernel needs, as numbers (for messages to web workers).
pub fn encode(l: &LightParams) -> Vec<f64> {
    vec![
        l.n_led as f64,
        l.n_led_live as f64,
        l.r_led,
        l.z_led,
        l.alpha0,
        l.falloff,
        l.shadow_mm,
        l.soft,
        l.ambient,
        l.ao,
        l.ao_mm,
        l.grain,
    ]
}

pub fn decode(v: &[f64]) -> LightParams {
    LightParams {
        n_led: v[0] as usize,
        n_led_live: v[1] as usize,
        r_led: v[2],
        z_led: v[3],
        alpha0: v[4],
        falloff: v[5],
        shadow_mm: v[6],
        soft: v[7],
        ambient: v[8],
        ao: v[9],
        ao_mm: v[10],
        grain: v[11],
        ..LightParams::default()
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
