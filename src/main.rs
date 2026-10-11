//! sandsim: live simulation of a kinetic sand table, as a web app (egui/wasm).

use sandsim::app;
#[cfg(not(target_arch = "wasm32"))]
use sandsim::{oblique, params, path, png, pool, shade, sim};

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    env_logger::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--bench") => {
            bench(&args[1]);
            return Ok(());
        }
        Some("--bench-live") => {
            bench_live(&args[1], 20.0);
            return Ok(());
        }
        _ => {}
    }
    let file = args.first().map(|p| {
        let name = std::path::Path::new(p).file_name().unwrap().to_string_lossy().into_owned();
        (name, std::fs::read(p).expect("cannot read pattern"))
    });
    eframe::run_native(
        "sandsim",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([1400.0, 1000.0]),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(app::SandApp::new(cc, file)))),
    )
}

/// Full simulation and lighting without a window, with timings and PNGs.
/// SANDSIM_WORKERS sets the number of lighting threads (0: main thread only).
#[cfg(not(target_arch = "wasm32"))]
fn bench(thr: &str) {
    use web_time::Instant;
    let p = params::SimParams::default();
    let pts = path::parse_thr(&std::fs::read_to_string(thr).unwrap()).unwrap();
    let t = Instant::now();
    let (bp, nc) = path::ball_path(&pts, &p);
    let mut sand = sim::Sand::new(&p, bp, nc);
    println!("setup: {:.2} s, {} steps", t.elapsed().as_secs_f64(), sand.path.len());
    let t = Instant::now();
    sand.advance(usize::MAX / 2);
    println!("simulation: {:.2} s", t.elapsed().as_secs_f64());
    let mut lp = params::LightParams::default();
    if let Ok(v) = std::env::var("N_LED") {
        lp.n_led = v.parse().unwrap();
    }
    let mut pool = pool::Pool::spawn(None);
    let t = Instant::now();
    let mut sh = shade::Shader::new(&sand, p.sand_mm, lp);
    while sh.pending() + sh.rough() > 0 {
        sh.work(&sand, sand.ball(), 1e9, false, pool.as_mut());
        if pool.is_some() {
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
    }
    let workers = pool.as_ref().map_or(0, |p| p.size());
    println!("lighting: {:.2} s ({workers} threads)", t.elapsed().as_secs_f64());
    let out = std::path::Path::new(thr).with_extension("").to_string_lossy().into_owned();
    std::fs::write(format!("{out}_rs.png"), png::encode(&sh.image)).unwrap();
    let t = Instant::now();
    let mut ob = oblique::Oblique::new(
        &params::Camera::default(),
        &sand,
        p.sand_mm,
        p.ball_r,
        sh.sand_color(),
    );
    ob.work(&sand, &sh.image, 1e9);
    println!("oblique: {:.2} s", t.elapsed().as_secs_f64());
    std::fs::write(format!("{out}_rs_oblique.png"), png::encode(&ob.image)).unwrap();
}

/// The animation loop of the app at "as fast as possible", without drawing: how fast does
/// the ball get when it waits for the lighting? Runs for `secs` seconds.
#[cfg(not(target_arch = "wasm32"))]
fn bench_live(thr: &str, secs: f64) {
    use web_time::Instant;
    let p = params::SimParams::default();
    let pts = path::parse_thr(&std::fs::read_to_string(thr).unwrap()).unwrap();
    let (bp, nc) = path::ball_path(&pts, &p);
    let mut sand = sim::Sand::new(&p, bp, nc);
    let mut sh = shade::Shader::new(&sand, p.sand_mm, params::LightParams::default());
    let mut pool = pool::Pool::spawn(None);
    let workers = pool.as_ref().map_or(0, |p| p.size());
    let max_dirty = app::max_dirty(workers);
    let (t0, mut frames) = (Instant::now(), 0);
    while t0.elapsed().as_secs_f64() < secs && !sand.done() {
        let t = Instant::now();
        sh.receive(pool.as_mut());
        let wait = sh.dirty() > 0;
        while !wait && t.elapsed().as_secs_f64() < 0.030 && sh.dirty() < max_dirty && !sand.done() {
            let dirty = sand.advance(16);
            sh.mark(dirty);
        }
        let sim_ms = t.elapsed().as_secs_f64() * 1e3;
        sh.work(&sand, sand.ball(), (34.0 - sim_ms).max(8.0), true, pool.as_mut());
        // a frame takes at least 1/60 s
        while t.elapsed().as_secs_f64() < 1.0 / 60.0 {
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        frames += 1;
    }
    let dt = t0.elapsed().as_secs_f64();
    println!(
        "live: {:.2} m in {dt:.1} s = {:.0} mm/s, {:.0} fps ({workers} threads)",
        sand.k as f64 * p.step / 1000.0,
        sand.k as f64 * p.step / dt,
        frames as f64 / dt
    );
}

#[cfg(target_arch = "wasm32")]
fn main() {
    use eframe::wasm_bindgen::JsCast as _;
    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    wasm_bindgen_futures::spawn_local(async {
        let document = web_sys::window().expect("no window").document().expect("no document");
        let canvas = document
            .get_element_by_id("the_canvas_id")
            .expect("canvas not found")
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .expect("not a canvas");
        let res = eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|cc| Ok(Box::new(app::SandApp::new(cc, None)))),
            )
            .await;
        if let Some(el) = document.get_element_by_id("loading") {
            match res {
                Ok(()) => el.remove(),
                Err(e) => el.set_inner_html(&format!("<p>The app crashed: {e:?}</p>")),
            }
        }
    });
}
