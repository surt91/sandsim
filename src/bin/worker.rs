//! Web worker that lights tiles for the web app (see `sandsim::pool`).

#[cfg(target_arch = "wasm32")]
fn main() {
    sandsim::worker::run();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("this binary only runs as a web worker");
}
