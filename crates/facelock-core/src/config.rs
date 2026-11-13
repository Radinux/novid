//! `/etc/facelock/config.toml`. Every field has a default so an empty or
//! missing file is valid.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::camera::CameraConfig;
use crate::embed::Normalize;

pub const DEFAULT_PATH: &str = "/etc/facelock/config.toml";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub camera: CameraConfig,
    pub models: ModelConfig,
    pub verify: VerifyConfig,
    pub storage: StorageConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelConfig {
    pub dir: PathBuf,
    pub detector: String,
    pub recognizer: String,
    pub normalize: Normalize,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            dir: "/usr/share/facelock/models".into(),
            detector: "face_detection_yunet_2026may.onnx".into(),
            recognizer: "face_recognition_sface_2021dec.onnx".into(),
            normalize: Normalize::RAW,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VerifyConfig {
    /// Cosine similarity needed for a frame to count as a match.
    pub threshold: f32,
    /// Matching frames needed before deciding.
    pub min_matches: u32,
    /// Two matching frames at or above this similarity are enough, without
    /// waiting for `min_matches`. 1.0 turns the shortcut off.
    pub fast_threshold: f32,
    pub timeout_ms: u64,
    /// Refuse when the IR emitter is not strobing (no lit/dark pairs).
    pub require_strobe: bool,
    /// Minimum brightening of the face when the emitter fires (0..255).
    pub min_emitter_gain: f32,
    /// Eye reflections must agree with the enrolled profile (glasses or not).
    pub eyewear_check: bool,
    /// Consecutive failures before a cooldown, and its length.
    pub max_failures: u32,
    pub cooldown_s: u64,
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            min_matches: 3,
            fast_threshold: 0.8,
            timeout_ms: 4000,
            require_strobe: true,
            min_emitter_gain: 30.0,
            // Experimental: the reflection classifier is not reliable yet.
            eyewear_check: false,
            max_failures: 5,
            cooldown_s: 30,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    pub dir: PathBuf,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self { dir: "/var/lib/facelock".into() }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }
}
