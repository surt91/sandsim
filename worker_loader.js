// Loads the lighting web worker (see src/pool.rs).
importScripts("./worker.js");
wasm_bindgen({ module_or_path: "./worker_bg.wasm" });
