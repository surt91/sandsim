//! Theta-rho path: parsing, clearing spiral, subdivision and the towed ball.

use crate::params::{Clear, SimParams};

/// Parses a `.thr` file (theta rho per line, `#` starts a comment).
pub fn parse_thr(text: &str) -> Result<Vec<[f64; 2]>, String> {
    let mut rows = Vec::new();
    for (no, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("");
        let mut it = line.split_whitespace();
        let (Some(a), Some(b)) = (it.next(), it.next()) else { continue };
        let parse = |s: &str| {
            s.parse::<f64>().map_err(|_| format!("line {}: '{}' is not a number", no + 1, s))
        };
        rows.push([parse(a)?, parse(b)?]);
    }
    if rows.len() < 2 {
        return Err("fewer than two points found".into());
    }
    Ok(rows)
}

/// Clearing spiral like clear_from_out/in (Archimedean, linear in theta/rho).
fn clear_path(start_out: bool, pitch_mm: f64, rho_mm: f64) -> Vec<[f64; 2]> {
    let turns = rho_mm / pitch_mm;
    let n = (turns * 200.0) as usize + 1;
    (0..=n)
        .map(|k| {
            let t = k as f64 / n as f64;
            [t * turns * std::f64::consts::TAU, if start_out { 1.0 - t } else { t }]
        })
        .collect()
}

/// Subdivides linearly in theta/rho (as the table moves), step ≈ step_mm.
fn resample(tr: &[[f64; 2]], step_mm: f64, rho_mm: f64) -> Vec<[f64; 2]> {
    let mut out = Vec::new();
    for w in tr.windows(2) {
        let (a, b) = (w[0], w[1]);
        let d = ((b[0] - a[0]) * a[1].max(b[1])).hypot(b[1] - a[1]);
        let n = ((d * rho_mm / step_mm).ceil() as usize).max(1);
        for k in 0..n {
            let f = k as f64 / n as f64;
            out.push([a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f]);
        }
    }
    out.push(*tr.last().unwrap());
    out
}

/// The ball is towed by the magnet and cuts corners (tractrix-like pursuit curve).
fn towed(xy: &mut [[f64; 2]], lag: f64) {
    let [mut bx, mut by] = xy[0];
    for p in xy.iter_mut() {
        let (dx, dy) = (p[0] - bx, p[1] - by);
        let d = dx.hypot(dy);
        if d > lag {
            bx += dx * (1.0 - lag / d);
            by += dy * (1.0 - lag / d);
        }
        *p = [bx, by];
    }
}

/// Ball positions in grid millimetres (origin top left, x right, y down),
/// plus the number of steps that belong to the clearing spiral.
pub fn ball_path(thr: &[[f64; 2]], p: &SimParams) -> (Vec<[f64; 2]>, usize) {
    let mut tr = Vec::new();
    let mut n_clear = 0;
    if p.clear {
        let from_out = match p.clear_dir {
            Clear::Auto => thr[0][1] < 0.5,
            c => c == Clear::Out,
        };
        let mut cp = clear_path(from_out, p.clear_pitch, p.rho_mm);
        // continue seamlessly into the pattern start
        let off = thr[0][0] - cp.last().unwrap()[0];
        cp.iter_mut().for_each(|q| q[0] += off);
        n_clear = resample(&cp, p.step, p.rho_mm).len();
        tr.extend(cp);
    }
    tr.extend_from_slice(thr);
    let mut xy: Vec<[f64; 2]> = resample(&tr, p.step, p.rho_mm)
        .into_iter()
        .map(|[th, rho]| {
            let r = rho.clamp(0.0, 1.0) * p.rho_mm;
            let y = r * th.sin();
            [r * th.cos(), if p.mirror { -y } else { y }]
        })
        .collect();
    if p.lag > 0.0 {
        towed(&mut xy, p.lag);
    }
    for q in xy.iter_mut() {
        *q = [q[0] + p.sand_mm, p.sand_mm - q[1]];
    }
    (xy, n_clear)
}
