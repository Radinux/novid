pub mod align;
pub mod camera;
pub mod config;
pub mod detect;
pub mod embed;
pub mod engine;
pub mod enroll;
pub mod feed;
pub mod image;
pub mod liveness;
pub mod pose;
pub mod store;
pub mod verify;

use std::path::Path;

use anyhow::Result;
use ort::environment::Environment;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;

/// Loads the system ONNX Runtime (`onnxruntime-cpu` on Arch).
pub fn ort_env() -> Result<Environment> {
    let lib = std::env::var("FACELOCK_ORT_LIB").unwrap_or_else(|_| "/usr/lib/libonnxruntime.so".into());
    Ok(ort::init_from(lib)?.with_name("facelock").build()?)
}

pub(crate) fn session(env: &Environment, path: &Path) -> Result<Session> {
    // Tuning options are best-effort: on failure, recover the builder and go on.
    Ok(Session::builder(env)?
        .with_optimization_level(GraphOptimizationLevel::All)
        .unwrap_or_else(|e| e.recover())
        .with_intra_threads(4)
        .unwrap_or_else(|e| e.recover())
        .commit_from_file(path)?)
}
