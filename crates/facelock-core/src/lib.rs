pub mod align;
pub mod antispoof;
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

/// Where ONNX Runtime is looked for, after `$FACELOCK_ORT_LIB`: the copy
/// bundled by the Debian package, then the system one (`onnxruntime` on Arch).
const ORT_LIBS: [&str; 2] = ["/usr/lib/facelock/libonnxruntime.so", "/usr/lib/libonnxruntime.so"];

pub fn ort_env() -> Result<Environment> {
    let lib = std::env::var("FACELOCK_ORT_LIB").unwrap_or_else(|_| {
        let found = ORT_LIBS.iter().find(|p| Path::new(p).exists());
        found.unwrap_or(&ORT_LIBS[1]).to_string()
    });
    Ok(ort::init_from(lib)?.with_name("facelock").build()?)
}

pub(crate) fn session(env: &Environment, path: &Path) -> Result<Session> {
    // Tuning options are best-effort: on failure, recover the builder and go on.
    Ok(Session::builder(env)?
        .with_optimization_level(GraphOptimizationLevel::All)
        .unwrap_or_else(|e| e.recover())
        .with_intra_threads(4)
        .unwrap_or_else(|e| e.recover())
        // Idle pool threads would otherwise busy-wait between frames: with
        // four models that kept several cores at 100% during a preview.
        .with_intra_op_spinning(false)
        .unwrap_or_else(|e| e.recover())
        .commit_from_file(path)?)
}
