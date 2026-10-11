//! Web workers that light tiles in parallel.
//!
//! Every worker keeps its own copy of the height field. The main thread sends it the
//! regions that changed and jobs (lists of tiles); messages to a worker are processed in
//! order, so a job always sees the height field as it was when the job was sent.
//!
//! Messages (JS arrays) to a worker:
//!   ["init", run, n, res, sand_mm, Float64Array params, Float32Array hs]
//!   ["params", Float64Array params]
//!   ["hs", i0, i1, j0, j1, Float32Array rows]
//!   ["job", run, Uint32Array [tile, i0, i1, j0, j1, fine, generation] * k]
//! and back:
//!   ["ready"]
//!   ["res", run, Uint32Array (as in the job), Float32Array [direct, ambient] per tile]

use crate::util::Rect;

pub struct TileJob {
    pub tile: usize,
    pub rect: Rect,
    pub fine: bool,
    pub generation: u32,
}

pub struct TileResult {
    pub run: u32,
    pub tile: usize,
    pub rect: Rect,
    pub fine: bool,
    pub generation: u32,
    pub direct: Vec<f32>,
    pub ambient: Vec<f32>,
}

/// Jobs a worker may have queued at once (more would make the priorities stale).
pub const MAX_OUTSTANDING: usize = 2;

#[cfg(target_arch = "wasm32")]
pub use imp::Pool;

#[cfg(not(target_arch = "wasm32"))]
pub use native::Pool;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::{TileJob, TileResult};
    use crate::params::LightParams;
    use crate::util::Rect;

    /// Natively everything is lit on the main thread.
    pub struct Pool;

    impl Pool {
        pub fn size(&self) -> usize {
            0
        }
        pub fn synced(&self) -> (u32, u32) {
            (0, 0)
        }
        #[allow(clippy::too_many_arguments)]
        pub fn start_run(
            &mut self,
            _: u32,
            _: u32,
            _: usize,
            _: f64,
            _: f64,
            _: &[f32],
            _: &LightParams,
        ) {
        }
        pub fn set_params(&mut self, _: u32, _: &LightParams) {}
        pub fn update(&mut self, _: Rect, _: &[f32], _: usize) {}
        pub fn free(&self) -> Vec<usize> {
            Vec::new()
        }
        pub fn send(&mut self, _: usize, _: u32, _: &[TileJob]) {}
        pub fn take(&mut self) -> Vec<TileResult> {
            Vec::new()
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::{MAX_OUTSTANDING, TileJob, TileResult};
    use crate::light::encode;
    use crate::params::LightParams;
    use crate::util::Rect;
    use eframe::wasm_bindgen::{JsCast, closure::Closure};
    use js_sys::{Array, Float32Array, Float64Array, Uint32Array};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    struct Worker {
        worker: web_sys::Worker,
        ready: Cell<bool>,
        /// messages sent before the worker was ready
        queue: RefCell<Vec<(Array, Array)>>,
        outstanding: Cell<usize>,
    }

    impl Worker {
        fn post(&self, msg: Array, transfer: Array) {
            if self.ready.get() {
                let _ = self.worker.post_message_with_transfer(&msg, &transfer);
            } else {
                self.queue.borrow_mut().push((msg, transfer));
            }
        }
    }

    pub struct Pool {
        workers: Vec<Rc<Worker>>,
        results: Rc<RefCell<Vec<TileResult>>>,
        /// (run, light parameter version) the workers know
        synced: (u32, u32),
    }

    impl Pool {
        /// Starts one worker per spare core; None if that isn't possible.
        pub fn new(ctx: &egui::Context) -> Option<Self> {
            let cores = web_sys::window()?.navigator().hardware_concurrency() as usize;
            let count = cores.saturating_sub(1).min(8);
            if count < 2 {
                return None;
            }
            let results: Rc<RefCell<Vec<TileResult>>> = Default::default();
            let mut workers = Vec::new();
            for _ in 0..count {
                let worker = web_sys::Worker::new("./worker_loader.js").ok()?;
                let w = Rc::new(Worker {
                    worker,
                    ready: Cell::new(false),
                    queue: Default::default(),
                    outstanding: Cell::new(0),
                });
                let (w2, results, ctx) = (w.clone(), results.clone(), ctx.clone());
                let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(
                    move |e: web_sys::MessageEvent| {
                        let msg: Array = e.data().unchecked_into();
                        match msg.get(0).as_string().as_deref() {
                            Some("ready") => {
                                w2.ready.set(true);
                                for (m, t) in w2.queue.borrow_mut().drain(..) {
                                    let _ = w2.worker.post_message_with_transfer(&m, &t);
                                }
                            }
                            Some("res") => {
                                w2.outstanding.set(w2.outstanding.get().saturating_sub(1));
                                parse_results(&msg, &mut results.borrow_mut());
                                ctx.request_repaint();
                            }
                            _ => {}
                        }
                    },
                );
                w.worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
                on_message.forget();
                workers.push(w);
            }
            log::info!("lighting with {count} web workers");
            Some(Self { workers, results, synced: (u32::MAX, 0) })
        }

        pub fn size(&self) -> usize {
            self.workers.len()
        }

        pub fn synced(&self) -> (u32, u32) {
            self.synced
        }

        #[allow(clippy::too_many_arguments)]
        pub fn start_run(
            &mut self,
            run: u32,
            version: u32,
            n: usize,
            res: f64,
            sand_mm: f64,
            hs: &[f32],
            l: &LightParams,
        ) {
            for w in &self.workers {
                let (params, hs) = (Float64Array::from(&encode(l)[..]), Float32Array::from(hs));
                let msg = Array::of5(
                    &"init".into(),
                    &run.into(),
                    &(n as u32).into(),
                    &res.into(),
                    &sand_mm.into(),
                );
                msg.push(&params);
                msg.push(&hs);
                w.post(msg, Array::of2(&params.buffer(), &hs.buffer()));
            }
            self.synced = (run, version);
        }

        pub fn set_params(&mut self, version: u32, l: &LightParams) {
            for w in &self.workers {
                let params = Float64Array::from(&encode(l)[..]);
                w.post(Array::of2(&"params".into(), &params), Array::of1(&params.buffer()));
            }
            self.synced.1 = version;
        }

        /// Sends the changed region of the height field to all workers.
        pub fn update(&mut self, r: Rect, hs: &[f32], n: usize) {
            let mut rows = Vec::with_capacity((r.i1 - r.i0) * (r.j1 - r.j0));
            for i in r.i0..r.i1 {
                rows.extend_from_slice(&hs[i * n + r.j0..i * n + r.j1]);
            }
            for w in &self.workers {
                let data = Float32Array::from(&rows[..]);
                let msg = Array::of5(
                    &"hs".into(),
                    &(r.i0 as u32).into(),
                    &(r.i1 as u32).into(),
                    &(r.j0 as u32).into(),
                    &(r.j1 as u32).into(),
                );
                msg.push(&data);
                w.post(msg, Array::of1(&data.buffer()));
            }
        }

        /// Workers that can take another job.
        pub fn free(&self) -> Vec<usize> {
            (0..self.workers.len())
                .filter(|&k| self.workers[k].outstanding.get() < MAX_OUTSTANDING)
                .collect()
        }

        pub fn send(&mut self, worker: usize, run: u32, jobs: &[TileJob]) {
            let meta: Vec<u32> = jobs
                .iter()
                .flat_map(|j| {
                    let r = j.rect;
                    [j.tile, r.i0, r.i1, r.j0, r.j1, j.fine as usize, j.generation as usize]
                        .map(|v| v as u32)
                })
                .collect();
            let meta = Uint32Array::from(&meta[..]);
            let w = &self.workers[worker];
            w.outstanding.set(w.outstanding.get() + 1);
            w.post(Array::of3(&"job".into(), &run.into(), &meta), Array::of1(&meta.buffer()));
        }

        pub fn take(&mut self) -> Vec<TileResult> {
            std::mem::take(&mut self.results.borrow_mut())
        }
    }

    fn parse_results(msg: &Array, out: &mut Vec<TileResult>) {
        let run = msg.get(1).as_f64().unwrap_or(-1.0) as u32;
        let meta = Uint32Array::from(msg.get(2)).to_vec();
        let data = Float32Array::from(msg.get(3)).to_vec();
        let mut off = 0;
        for m in meta.as_chunks::<7>().0 {
            let rect =
                Rect { i0: m[1] as usize, i1: m[2] as usize, j0: m[3] as usize, j1: m[4] as usize };
            let len = (rect.i1 - rect.i0) * (rect.j1 - rect.j0);
            out.push(TileResult {
                run,
                tile: m[0] as usize,
                rect,
                fine: m[5] != 0,
                generation: m[6],
                direct: data[off..off + len].to_vec(),
                ambient: data[off + len..off + 2 * len].to_vec(),
            });
            off += 2 * len;
        }
    }
}
