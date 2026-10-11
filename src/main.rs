//! sandsim-rs: live simulation of a kinetic sand table, as a web app (egui/wasm).

mod app;
mod oblique;
mod params;
mod path;
mod png;
mod shade;
mod sim;
mod util;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    env_logger::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--bench") {
        bench(&args[1]);
        return Ok(());
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

/// Full simulation and lighting without a window, with timings and a PNG.
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
    let t = Instant::now();
    let mut lp = params::LightParams::default();
    if let Ok(v) = std::env::var("N_LED") {
        lp.n_led = v.parse().unwrap();
    }
    let mut sh = shade::Shader::new(&sand, p.sand_mm, lp);
    sh.work(&sand, sand.ball(), 1e9, false);
    println!("lighting: {:.2} s", t.elapsed().as_secs_f64());
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
