//! egui front end: load a pattern, watch it being drawn, tweak the parameters.

use crate::oblique::Oblique;
use crate::params::{Camera, Clear, LightParams, SimParams};
use crate::path::{ball_path, parse_thr};
use crate::pool::Pool;
use crate::shade::Shader;
use crate::sim::Sand;
use egui::{Color32, Pos2, Rect, Sense, TextureHandle, TextureOptions, Vec2};
use std::sync::{Arc, Mutex};
use web_time::Instant;

const EXAMPLES: &[(&str, &str)] = &[
    ("Zen garden (8 mm pitch)", include_str!("../prototype/examples/zen_garten_eng.thr")),
    ("Parasaurolophus", include_str!("../prototype/examples/parasaurolophus.thr")),
    ("Spirograph", include_str!("../assets/spirograph.thr")),
    ("Sunflower", include_str!("../assets/sonnenblume.thr")),
];

/// file name and contents
type NamedFile = (String, Vec<u8>);

#[derive(PartialEq, Clone, Copy)]
enum View {
    Top,
    Oblique,
}

struct Run {
    sand: Sand,
    shader: Shader,
    params: SimParams,
    tex: Option<TextureHandle>,
    oblique: Option<(Oblique, TextureHandle, (usize, u64, Camera))>,
}

pub struct SandApp {
    name: String,
    thr: Vec<[f64; 2]>,
    error: Option<String>,
    sim: SimParams,
    light: LightParams,
    light_version: u64,
    cam: Camera,
    run: Option<Run>,
    /// web workers for the lighting (web only)
    pool: Option<Pool>,
    playing: bool,
    speed: f64, // ball speed [mm/s]
    max_speed: bool,
    /// fast-forward (simulation only, lighting afterwards) up to this step
    skip_to: Option<usize>,
    step_acc: f64,
    view: View,
    zoom: f32,
    pan: Vec2,
    incoming: Arc<Mutex<Option<NamedFile>>>,
    sim_ms: f64,
    sim_steps: usize,
    actual_speed: f64,
    status: String,
}

impl SandApp {
    pub fn new(cc: &eframe::CreationContext<'_>, file: Option<(String, Vec<u8>)>) -> Self {
        let mut app = Self {
            name: String::new(),
            thr: Vec::new(),
            error: None,
            sim: SimParams::default(),
            light: LightParams::default(),
            light_version: 0,
            cam: Camera::default(),
            run: None,
            #[cfg(target_arch = "wasm32")]
            pool: Pool::new(&cc.egui_ctx),
            #[cfg(not(target_arch = "wasm32"))]
            pool: {
                let _ = cc;
                None
            },
            playing: true,
            speed: 3000.0,
            max_speed: false,
            skip_to: None,
            step_acc: 0.0,
            view: View::Top,
            zoom: 1.0,
            pan: Vec2::ZERO,
            incoming: Default::default(),
            sim_ms: 0.0,
            sim_steps: 0,
            actual_speed: 0.0,
            status: String::new(),
        };
        match file {
            Some((name, bytes)) => app.load(name, &bytes),
            None => app.load(EXAMPLES[0].0.into(), EXAMPLES[0].1.as_bytes()),
        }
        app
    }

    fn load(&mut self, name: String, bytes: &[u8]) {
        match parse_thr(&String::from_utf8_lossy(bytes)) {
            Ok(thr) => {
                self.name = name;
                self.thr = thr;
                self.error = None;
                self.restart();
            }
            Err(e) => self.error = Some(format!("{name}: {e}")),
        }
    }

    fn restart(&mut self) {
        let (path, n_clear) = ball_path(&self.thr, &self.sim);
        let sand = Sand::new(&self.sim, path, n_clear);
        let shader = Shader::new(&sand, self.sim.sand_mm, self.light.clone());
        self.run = Some(Run { sand, shader, params: self.sim.clone(), tex: None, oblique: None });
        self.skip_to = None;
        self.step_acc = 0.0;
        self.playing = true;
    }

    /// Opens the browser's file picker (a hidden `<input type=file>`).
    #[cfg(target_arch = "wasm32")]
    fn open_dialog(&self, ctx: &egui::Context) {
        use eframe::wasm_bindgen::{JsCast, closure::Closure};
        let doc = web_sys::window().unwrap().document().unwrap();
        let Ok(input) = doc.create_element("input") else { return };
        let input: web_sys::HtmlInputElement = input.unchecked_into();
        input.set_type("file");
        input.set_accept(".thr,.txt");
        let (incoming, ctx, inp) = (self.incoming.clone(), ctx.clone(), input.clone());
        let on_change = Closure::once_into_js(move || {
            let file = inp.files().and_then(|f| f.get(0));
            inp.remove();
            let Some(file) = file else { return };
            wasm_bindgen_futures::spawn_local(async move {
                let text = wasm_bindgen_futures::JsFuture::from(file.text()).await;
                if let Some(text) = text.ok().and_then(|t| t.as_string()) {
                    *incoming.lock().unwrap() = Some((file.name(), text.into_bytes()));
                    ctx.request_repaint();
                }
            });
        });
        input.set_onchange(Some(on_change.unchecked_ref()));
        // some browsers only open the picker for inputs that are part of the document
        let _ = input.style().set_property("display", "none");
        if let Some(body) = doc.body() {
            let _ = body.append_child(&input);
        }
        input.click();
    }

    /// Simulation and lighting for this frame, within a time budget.
    fn step(&mut self, dt: f64) {
        let Some(run) = &mut self.run else { return };
        let t0 = Instant::now();
        let step_mm = run.params.step;
        if let Some(k) = self.skip_to
            && (run.sand.k >= k || run.sand.done())
        {
            self.skip_to = None;
        }
        let k0 = run.sand.k;
        if !run.sand.done() && (self.playing || self.skip_to.is_some()) {
            let fast = self.max_speed || self.skip_to.is_some();
            let budget = if fast { 30.0 } else { 14.0 };
            let mut want = if fast {
                usize::MAX
            } else {
                self.step_acc += self.speed * dt / step_mm;
                let w = self.step_acc.floor();
                self.step_acc -= w;
                w as usize
            };
            if let Some(k) = self.skip_to {
                want = want.min(k.saturating_sub(run.sand.k));
            }
            // the animation waits for the lighting, so that it shows what is drawn
            let workers = self.pool.as_ref().map_or(0, |p| p.size());
            let max_dirty = if self.skip_to.is_some() { usize::MAX } else { 8 + 2 * workers };
            while want > 0
                && t0.elapsed().as_secs_f64() * 1e3 < budget
                && run.shader.dirty() < max_dirty
            {
                let s = want.min(16);
                let dirty = run.sand.advance(s);
                run.shader.mark(dirty);
                want -= s;
            }
            if want > 0 && !fast {
                // can't keep up
                self.step_acc = 0.0; // don't build a backlog
            }
        }
        self.sim_ms = t0.elapsed().as_secs_f64() * 1e3;
        self.sim_steps = run.sand.k - k0;
        if dt > 0.0 {
            let v = self.sim_steps as f64 * step_mm / dt;
            self.actual_speed += (v - self.actual_speed) * (dt / 0.5).min(1.0);
        }
        let budget = if self.skip_to.is_some() { 4.0 } else { (34.0 - self.sim_ms).max(8.0) };
        let live = !run.sand.done() && (self.playing || self.skip_to.is_some());
        run.shader.work(&run.sand, run.sand.ball(), budget, live, self.pool.as_mut());
    }

    fn upload(&mut self, ctx: &egui::Context) {
        let Some(run) = &mut self.run else { return };
        let opts = TextureOptions::LINEAR;
        let sh = &mut run.shader;
        match &mut run.tex {
            None => {
                run.tex = Some(ctx.load_texture("sand", sh.image.clone(), opts));
                sh.full_change = false;
                sh.changed.clear();
            }
            Some(tex) => {
                if sh.full_change || sh.changed.len() > 60 {
                    tex.set(sh.image.clone(), opts);
                } else {
                    for &t in &sh.changed {
                        let r = sh.tile_rect(t);
                        let sub =
                            sh.image.region_by_pixels([r.j0, r.i0], [r.j1 - r.j0, r.i1 - r.i0]);
                        tex.set_partial([r.j0, r.i0], sub, opts);
                    }
                }
                sh.full_change = false;
                sh.changed.clear();
            }
        }
    }

    fn oblique(&mut self, ctx: &egui::Context) {
        let Some(run) = &mut self.run else { return };
        if self.view != View::Oblique {
            return;
        }
        let key = (run.sand.k, self.light_version, self.cam.clone());
        let idle = (!self.playing || run.sand.done()) && self.skip_to.is_none();
        let lit = run.shader.pending() + run.shader.rough() == 0;
        let stale = run.oblique.as_ref().is_none_or(|o| o.2 != key);
        if stale && lit && (idle || run.oblique.is_none()) {
            let sc = run.shader.sand_color();
            let o = Oblique::new(&self.cam, &run.sand, run.params.sand_mm, run.params.ball_r, sc);
            let tex = ctx.load_texture("oblique", o.image.clone(), TextureOptions::LINEAR);
            run.oblique = Some((o, tex, key));
        }
        if let Some((o, tex, _)) = &mut run.oblique
            && !o.done()
        {
            o.work(&run.sand, &run.shader.image, 30.0);
            tex.set(o.image.clone(), TextureOptions::LINEAR);
        }
    }

    fn busy(&self) -> bool {
        let Some(run) = &self.run else { return false };
        (!run.sand.done() && (self.playing || self.skip_to.is_some()))
            || run.shader.pending() + run.shader.rough() > 0
            || (self.view == View::Oblique
                && run.oblique.as_ref().is_none_or(|o| !o.0.done() || o.2.0 != run.sand.k))
    }

    fn export(&self) {
        let Some(run) = &self.run else { return };
        let (img, suffix) = match (&run.oblique, self.view) {
            (Some((o, _, _)), View::Oblique) => (&o.image, "_sand_oblique.png"),
            _ => (&run.shader.image, "_sand.png"),
        };
        let stem = self.name.trim_end_matches(".thr").replace(' ', "_");
        save_file(&format!("{stem}{suffix}"), &crate::png::encode(img));
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.heading("sandsim");
        ui.label("Simulated sand table (Dune Weaver Pro, 10 mm ball)");
        ui.separator();

        // --- pattern
        ui.strong("Pattern");
        ui.horizontal(|ui| {
            #[cfg(target_arch = "wasm32")]
            if ui.button("📂 Open .thr …").clicked() {
                self.open_dialog(ui.ctx());
            }
            egui::ComboBox::from_id_salt("examples").selected_text("Examples").show_ui(ui, |ui| {
                for (name, text) in EXAMPLES {
                    if ui.selectable_label(self.name == *name, *name).clicked() {
                        self.load((*name).into(), text.as_bytes());
                    }
                }
            });
        });
        ui.small("… or drop a .thr file onto the window.");
        ui.label(format!("{}  ({} points)", self.name, self.thr.len()));
        if let Some(e) = &self.error {
            ui.colored_label(Color32::LIGHT_RED, e);
        }
        ui.separator();

        // --- playback
        ui.strong("Playback");
        if let Some(run) = &self.run {
            let (k, total, nc) = (run.sand.k, run.sand.path.len(), run.sand.n_clear);
            let step = run.params.step;
            let phase = if k < nc { "clearing" } else { "pattern" };
            ui.add(egui::ProgressBar::new(k as f32 / total as f32).text(format!(
                "{phase}: {:.1} / {:.1} m",
                k as f64 * step / 1000.0,
                total as f64 * step / 1000.0
            )));
            let done = run.sand.done();
            let k_clear = nc;
            ui.horizontal(|ui| {
                let label = if self.playing && !done { "⏸ Pause" } else { "▶ Play" };
                if ui.add_enabled(!done, egui::Button::new(label)).clicked() {
                    self.playing = !self.playing;
                }
                if ui.button("⟲ Restart").clicked() {
                    self.restart();
                }
                if ui
                    .add_enabled(k < k_clear, egui::Button::new("⏭ Skip clearing"))
                    .on_hover_text("Simulate the clearing spiral without animation")
                    .clicked()
                {
                    self.skip_to = Some(k_clear);
                }
                if ui
                    .add_enabled(!done, egui::Button::new("⏩ Finish"))
                    .on_hover_text("Simulate the rest without animation, then light it")
                    .clicked()
                {
                    self.skip_to = Some(usize::MAX);
                }
            });
        }
        ui.horizontal(|ui| {
            ui.label("Speed");
            ui.add_enabled(
                !self.max_speed,
                egui::Slider::new(&mut self.speed, 20.0..=100_000.0)
                    .logarithmic(true)
                    .suffix(" mm/s")
                    .max_decimals(0),
            );
        });
        ui.checkbox(&mut self.max_speed, "as fast as possible");
        if let Some(run) = &self.run {
            let (p, r) = (run.shader.pending(), run.shader.rough());
            let t = run.shader.total_tiles();
            if p > 0 {
                ui.add(
                    egui::ProgressBar::new(1.0 - p as f32 / t as f32)
                        .text(format!("lighting: {p} of {t} tiles pending")),
                );
            } else if r > 0 {
                ui.add(
                    egui::ProgressBar::new(1.0 - r as f32 / t as f32)
                        .text(format!("refining light: {r} tiles")),
                );
            } else {
                // keep the layout steady
                ui.add(egui::ProgressBar::new(1.0).text("lighting complete"));
            }
        }
        ui.small(egui::RichText::new(&self.status).weak());
        ui.separator();

        // --- view
        ui.strong("View");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.view, View::Top, "Top view");
            ui.selectable_value(&mut self.view, View::Oblique, "Oblique view");
            if ui.button("💾 PNG").on_hover_text("Save the current image").clicked() {
                self.export();
            }
        });
        if self.view == View::Oblique {
            ui.add(egui::Slider::new(&mut self.cam.rot, -180.0..=180.0).text("camera rotation °"));
            ui.horizontal(|ui| {
                ui.label("size");
                for (w, h) in [(680, 512), (1020, 768), (2040, 1536)] {
                    if ui.selectable_label(self.cam.width == w, format!("{w}×{h}")).clicked() {
                        self.cam.width = w;
                        self.cam.height = h;
                    }
                }
            });
            ui.small("Rendered when paused or finished.");
            if let Some(Some((o, _, _))) = self.run.as_ref().map(|r| &r.oblique)
                && !o.done()
            {
                ui.add(
                    egui::ProgressBar::new(o.row as f32 / o.image.size[1] as f32).text("rendering"),
                );
            }
        } else {
            ui.small("Scroll to zoom, drag to pan, double-click to reset.");
        }
        ui.separator();

        egui::ScrollArea::vertical().show(ui, |ui| {
            self.light_controls(ui);
            self.sim_controls(ui);
        });
    }

    fn light_controls(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Lighting").default_open(true).show(ui, |ui| {
            let l = &mut self.light;
            let mut c = false;
            c |= ui
                .add(
                    egui::Slider::new(&mut l.exposure, 0.3..=3.0)
                        .logarithmic(true)
                        .text("exposure"),
                )
                .changed();
            ui.horizontal(|ui| {
                ui.label("LED colour");
                c |= ui.radio_value(&mut l.led_color, None, "orange (calibrated)").changed();
                let mut rgb = l.led_color.unwrap_or([255, 255, 255]);
                let custom = l.led_color.is_some();
                if ui.radio(custom, "custom").clicked() && !custom {
                    l.led_color = Some(rgb);
                    c = true;
                }
                if ui.color_edit_button_srgb(&mut rgb).changed() {
                    l.led_color = Some(rgb);
                    c = true;
                }
            });
            ui.horizontal(|ui| {
                for (name, rgb) in [
                    ("white", [255, 255, 255]),
                    ("warm white", [255, 196, 137]),
                    ("cool white", [215, 230, 255]),
                    ("blue", [60, 110, 255]),
                    ("green", [70, 255, 120]),
                ] {
                    if ui.small_button(name).clicked() {
                        l.led_color = Some(rgb);
                        c = true;
                    }
                }
            });
            c |= ui
                .add(egui::Slider::new(&mut l.z_led, 3.0..=100.0).text("LED height [mm]"))
                .changed();
            c |= ui
                .add(egui::Slider::new(&mut l.alpha0, 5.0..=90.0).text("beam angle [°]"))
                .changed();
            c |= ui
                .add(egui::Slider::new(&mut l.n_led, 4..=96).text("LEDs (strip sampling)"))
                .changed();
            c |= ui
                .add(egui::Slider::new(&mut l.n_led_live, 4..=96).text("LEDs while drawing"))
                .on_hover_text(
                    "Fewer LEDs light faster; tiles are refined with all LEDs afterwards",
                )
                .changed();
            c |= ui
                .add(egui::Slider::new(&mut l.falloff, 0.0..=3.0).text("falloff exponent"))
                .changed();
            c |= ui
                .add(egui::Slider::new(&mut l.shadow_mm, 5.0..=100.0).text("max. shadow [mm]"))
                .changed();
            c |= ui.add(egui::Slider::new(&mut l.soft, 0.2..=10.0).text("penumbra [°]")).changed();
            c |= ui
                .add(egui::Slider::new(&mut l.ambient, 0.0..=2.0).text("ambient light"))
                .changed();
            c |= ui
                .add(egui::Slider::new(&mut l.ao, 0.0..=8.0).text("occlusion exponent"))
                .changed();
            c |= ui.add(egui::Slider::new(&mut l.grain, 0.0..=0.1).text("grain [mm]")).changed();
            c |= ui
                .add(egui::Slider::new(&mut l.albedo_noise, 0.0..=0.5).text("colour noise"))
                .changed();
            if ui.button("Defaults").clicked() {
                *l = LightParams::default();
                c = true;
            }
            if c {
                self.light_version += 1;
                if let Some(run) = &mut self.run {
                    run.shader.set_params(self.light.clone());
                }
            }
        });
    }

    fn sim_controls(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Sand & table").default_open(true).show(ui, |ui| {
            let p = &mut self.sim;
            ui.horizontal(|ui| {
                ui.label("clearing");
                for (c, name) in [
                    (Clear::Auto, "auto"),
                    (Clear::In, "in"),
                    (Clear::Out, "out"),
                    (Clear::None, "none"),
                ] {
                    ui.selectable_value(&mut p.clear, c, name);
                }
            });
            ui.checkbox(&mut p.mirror, "mirror theta");
            ui.add(egui::Slider::new(&mut p.h0, 1.0..=10.0).text("sand depth [mm]"));
            ui.add(egui::Slider::new(&mut p.repose, 10.0..=45.0).text("angle of repose [°]"));
            ui.add(egui::Slider::new(&mut p.stat, 15.0..=60.0).text("max. stable slope [°]"));
            ui.add(egui::Slider::new(&mut p.smooth, 0.0..=1.5).text("edge rounding [mm]"));
            ui.add(egui::Slider::new(&mut p.push_w, 0.5..=6.0).text("bulge width [mm]"));
            ui.add(egui::Slider::new(&mut p.side, 0.0..=2.0).text("sideways share"));
            ui.add(egui::Slider::new(&mut p.lag, 0.0..=20.0).text("ball lag [mm]"));
            ui.add(egui::Slider::new(&mut p.ball_r, 2.0..=12.0).text("ball radius [mm]"));
            ui.add(egui::Slider::new(&mut p.clear_pitch, 3.0..=15.0).text("clearing pitch [mm]"));
            ui.add(egui::Slider::new(&mut p.rho_mm, 100.0..=400.0).text("rho = 1 [mm]"));
            ui.add(egui::Slider::new(&mut p.sand_mm, 100.0..=420.0).text("sand radius [mm]"));
            ui.add(egui::Slider::new(&mut p.res, 0.25..=1.5).text("grid [mm/px]"))
                .on_hover_text("Finer grids are slower; the model is calibrated at 0.5 mm/px");
            ui.add(egui::Slider::new(&mut p.step, 0.25..=2.0).text("ball step [mm]"));
            p.stat = p.stat.max(p.repose);
            p.sand_mm = p.sand_mm.max(p.rho_mm);
            ui.horizontal(|ui| {
                let changed = self.run.as_ref().is_some_and(|r| r.params != self.sim);
                let btn = egui::Button::new("Apply & restart");
                let btn = if changed { btn.fill(Color32::from_rgb(120, 70, 30)) } else { btn };
                if ui.add(btn).clicked() {
                    self.restart();
                }
                if ui.button("Defaults").clicked() {
                    self.sim = SimParams::default();
                }
            });
        });
    }

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let avail = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(avail, Sense::click_and_drag());
        ui.painter().rect_filled(avail, 0.0, Color32::from_rgb(9, 6, 6));
        let Some(run) = &self.run else { return };

        if self.view == View::Oblique {
            if let Some((o, tex, _)) = &run.oblique {
                let [w, h] = o.image.size;
                let s = (avail.width() / w as f32).min(avail.height() / h as f32);
                let r = Rect::from_center_size(avail.center(), Vec2::new(w as f32, h as f32) * s);
                ui.painter().image(
                    tex.id(),
                    r,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            } else {
                ui.painter().text(
                    avail.center(),
                    egui::Align2::CENTER_CENTER,
                    "waiting for the lighting …",
                    egui::FontId::proportional(16.0),
                    Color32::GRAY,
                );
            }
            return;
        }

        // zoom & pan
        if resp.hovered() {
            let (scroll, zd) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let f = zd * (scroll * 0.002).exp();
            if f != 1.0 {
                let ptr = ui.input(|i| i.pointer.hover_pos()).unwrap_or(avail.center());
                let new = (self.zoom * f).clamp(1.0, 30.0);
                let f = new / self.zoom;
                self.pan = (self.pan - (ptr - avail.center())) * f + (ptr - avail.center());
                self.zoom = new;
            }
        }
        if resp.dragged() {
            self.pan += resp.drag_delta();
        }
        if resp.double_clicked() {
            self.zoom = 1.0;
            self.pan = Vec2::ZERO;
        }
        let side = avail.width().min(avail.height()) * self.zoom;
        let r = Rect::from_center_size(avail.center() + self.pan, Vec2::splat(side));
        let painter = ui.painter_at(avail);
        if let Some(tex) = &run.tex {
            painter.image(
                tex.id(),
                r,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        // the steel ball
        let n = run.sand.n as f32;
        let [bx, by] = run.sand.ball();
        let res = run.params.res as f32;
        let c = r.min + Vec2::new(bx as f32 / res + 0.5, by as f32 / res + 0.5) / n * side;
        let br = run.params.ball_r as f32 / res / n * side;
        // polished steel: dark surroundings, sand reflected towards the rim, a highlight
        let s = run.shader.sand_rgb;
        let shade = |k: f64, add: f64| {
            let c = s.map(|v| ((v * k + add).clamp(0.0, 1.0) * 255.0) as u8);
            Color32::from_rgb(c[0], c[1], c[2])
        };
        painter.circle_filled(c, br, shade(0.3, 0.03));
        painter.circle_filled(c + Vec2::new(br * 0.1, br * 0.15), br * 0.75, shade(0.42, 0.03));
        painter.circle_filled(c + Vec2::new(br * 0.3, br * 0.35), br * 0.4, shade(0.65, 0.03));
        painter.circle_filled(c + Vec2::new(br * 0.25, -br * 0.2), br * 0.15, shade(0.15, 0.82));
    }
}

impl eframe::App for SandApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // incoming files: dialog or drag & drop
        let incoming = self.incoming.lock().unwrap().take();
        if let Some((name, bytes)) = incoming {
            self.load(name, &bytes);
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            let name = f.path().file_name().map(|s| s.to_string_lossy().into_owned());
            let name = name.unwrap_or_default();
            #[cfg(not(target_arch = "wasm32"))]
            match f.bytes() {
                Ok(b) => self.load(name, &b),
                Err(e) => self.error = Some(e),
            }
            #[cfg(target_arch = "wasm32")]
            {
                let incoming = self.incoming.clone();
                let ctx = ctx.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    if let Ok(b) = f.bytes_async().await {
                        *incoming.lock().unwrap() = Some((name, b));
                        ctx.request_repaint();
                    }
                });
            }
        }

        let t0 = Instant::now();
        let dt = ctx.input(|i| i.stable_dt).min(0.1) as f64;
        self.step(dt);
        self.upload(&ctx);
        self.oblique(&ctx);
        if let Some(run) = &self.run {
            let (lt, lms) = run.shader.stats;
            let light = match run.shader.workers {
                0 => format!("light {lt} tiles in {lms:.0} ms"),
                w => format!("light {lt} tiles from {w} workers"),
            };
            self.status = format!(
                "{:.0} mm/s · grid {n}×{n} · sim {} steps in {:.0} ms · {light} · frame {:.0} ms",
                self.actual_speed,
                self.sim_steps,
                self.sim_ms,
                t0.elapsed().as_secs_f64() * 1e3,
                n = run.sand.n,
            );
        }

        egui::Panel::left("controls").default_size(330.0).show(ui, |ui| self.controls(ui));
        egui::CentralPanel::no_frame().show(ui, |ui| self.canvas(ui));

        if self.busy() {
            ctx.request_repaint();
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn save_file(name: &str, data: &[u8]) {
    use eframe::wasm_bindgen::JsCast;
    let arr = js_sys::Uint8Array::from(data);
    let parts = js_sys::Array::of1(&arr);
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type("image/png");
    let Ok(blob) = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &opts) else {
        return;
    };
    let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else { return };
    let doc = web_sys::window().unwrap().document().unwrap();
    if let Ok(a) = doc.create_element("a") {
        let a: web_sys::HtmlAnchorElement = a.unchecked_into();
        a.set_href(&url);
        a.set_download(name);
        a.click();
    }
    let _ = web_sys::Url::revoke_object_url(&url);
}

#[cfg(not(target_arch = "wasm32"))]
fn save_file(name: &str, data: &[u8]) {
    match std::fs::write(name, data) {
        Ok(()) => log::info!("saved: {name}"),
        Err(e) => log::error!("{name}: {e}"),
    }
}
