//! The web worker side of [`crate::pool`]: lights tiles on request.

use crate::light::{Grid, Kernel, decode};
use crate::util::Rect;
use eframe::wasm_bindgen::{JsCast, closure::Closure};
use js_sys::{Array, Float32Array, Float64Array, Uint32Array};
use std::cell::RefCell;
use web_sys::DedicatedWorkerGlobalScope;

struct State {
    run: u32,
    grid: Grid,
    kernel: Kernel,
    hs: Vec<f32>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Entry point of the worker binary.
pub fn run() {
    let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
    let on_message =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|e: web_sys::MessageEvent| {
            let msg: Array = e.data().unchecked_into();
            STATE.with(|s| handle(&msg, &mut s.borrow_mut()));
        });
    scope.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();
    let _ = scope.post_message(&Array::of1(&"ready".into()));
}

fn num(msg: &Array, k: u32) -> f64 {
    msg.get(k).as_f64().unwrap_or(0.0)
}

fn handle(msg: &Array, state: &mut Option<State>) {
    match msg.get(0).as_string().as_deref() {
        Some("init") => {
            let (n, res, sand_mm) = (num(msg, 2) as usize, num(msg, 3), num(msg, 4));
            let l = decode(&Float64Array::from(msg.get(5)).to_vec());
            *state = Some(State {
                run: num(msg, 1) as u32,
                grid: Grid::new(n, res, sand_mm),
                kernel: Kernel::new(&l, res),
                hs: Float32Array::from(msg.get(6)).to_vec(),
            });
        }
        Some("params") => {
            if let Some(s) = state {
                let l = decode(&Float64Array::from(msg.get(1)).to_vec());
                s.kernel = Kernel::new(&l, s.grid.res);
            }
        }
        Some("hs") => {
            if let Some(s) = state {
                let [i0, i1, j0, j1] = [1, 2, 3, 4].map(|k| num(msg, k) as usize);
                let rows = Float32Array::from(msg.get(5)).to_vec();
                let (n, w) = (s.grid.n, j1 - j0);
                for i in i0..i1 {
                    s.hs[i * n + j0..i * n + j1].copy_from_slice(&rows[(i - i0) * w..][..w]);
                }
            }
        }
        Some("job") => {
            let run = num(msg, 1) as u32;
            let mut meta = Uint32Array::from(msg.get(2)).to_vec();
            // always answer, the main thread counts outstanding jobs
            let Some(s) = state.as_ref().filter(|s| s.run == run) else {
                meta.clear();
                return reply(run, &meta, &[]);
            };
            let mut data = Vec::new();
            for m in meta.as_chunks::<7>().0 {
                let r = Rect {
                    i0: m[1] as usize,
                    i1: m[2] as usize,
                    j0: m[3] as usize,
                    j1: m[4] as usize,
                };
                let len = (r.i1 - r.i0) * (r.j1 - r.j0);
                let (mut direct, mut ambient) = (vec![0.0; len], vec![0.0; len]);
                s.kernel.light(&s.grid, &s.hs, r, m[5] != 0, &mut direct, &mut ambient);
                data.extend(direct);
                data.extend(ambient);
            }
            reply(run, &meta, &data);
        }
        _ => {}
    }
}

fn reply(run: u32, meta: &[u32], data: &[f32]) {
    let (meta, data) = (Uint32Array::from(meta), Float32Array::from(data));
    let out = Array::of4(&"res".into(), &run.into(), &meta, &data);
    let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
    let _ = scope.post_message_with_transfer(&out, &Array::of2(&meta.buffer(), &data.buffer()));
}
