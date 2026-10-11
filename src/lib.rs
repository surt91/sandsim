//! sandsim: live simulation of a kinetic sand table, as a web app (egui/wasm).

pub mod app;
pub mod light;
pub mod oblique;
pub mod params;
pub mod path;
pub mod png;
pub mod pool;
pub mod shade;
pub mod sim;
pub mod util;
#[cfg(target_arch = "wasm32")]
pub mod worker;
