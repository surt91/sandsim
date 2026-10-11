//! The web worker side of [`crate::pool`]: lights tiles on request.

use crate::light::decode;
use crate::pool::{TileJob, WorkerState};
use crate::util::Rect;
use eframe::wasm_bindgen::{JsCast, closure::Closure};
use js_sys::{Array, Float32Array, Float64Array, Uint32Array};
use std::cell::RefCell;
use web_sys::DedicatedWorkerGlobalScope;

thread_local! {
    static STATE: RefCell<Option<WorkerState>> = const { RefCell::new(None) };
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

fn handle(msg: &Array, state: &mut Option<WorkerState>) {
    match msg.get(0).as_string().as_deref() {
        Some("init") => {
            let (run, n, res, sand_mm) =
                (num(msg, 1) as u32, num(msg, 2) as usize, num(msg, 3), num(msg, 4));
            let l = decode(&Float64Array::from(msg.get(5)).to_vec());
            let hs = Float32Array::from(msg.get(6)).to_vec();
            *state = Some(WorkerState::new(run, n, res, sand_mm, &l, hs));
        }
        Some("params") => {
            if let Some(s) = state {
                s.set_params(&decode(&Float64Array::from(msg.get(1)).to_vec()));
            }
        }
        Some("hs") => {
            if let Some(s) = state {
                let [i0, i1, j0, j1] = [1, 2, 3, 4].map(|k| num(msg, k) as usize);
                s.update(Rect { i0, i1, j0, j1 }, &Float32Array::from(msg.get(5)).to_vec());
            }
        }
        Some("job") => {
            let run = num(msg, 1) as u32;
            let meta = Uint32Array::from(msg.get(2)).to_vec();
            let jobs: Vec<TileJob> = meta
                .as_chunks::<7>()
                .0
                .iter()
                .map(|m| TileJob {
                    tile: m[0] as usize,
                    rect: Rect {
                        i0: m[1] as usize,
                        i1: m[2] as usize,
                        j0: m[3] as usize,
                        j1: m[4] as usize,
                    },
                    fine: m[5] != 0,
                    generation: m[6],
                })
                .collect();
            // always answer (possibly empty), the main thread counts outstanding jobs
            let results = state.as_ref().map_or(Vec::new(), |s| s.job(run, &jobs));
            let mut meta = Vec::new();
            let mut data = Vec::new();
            for r in results {
                let q = r.rect;
                meta.extend([r.tile, q.i0, q.i1, q.j0, q.j1, r.fine as usize].map(|v| v as u32));
                meta.push(r.generation);
                data.extend(r.direct);
                data.extend(r.ambient);
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
