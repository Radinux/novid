//! Models + per-frame analysis, shared by the daemon and the CLI.

use anyhow::Result;
use log::warn;

use crate::antispoof::AntiSpoof;
use crate::camera::FramePair;
use crate::config::ModelConfig;
use crate::detect::{Detector, Face};
use crate::embed::{Embedder, Embedding};
use crate::liveness::{self, LivenessFeatures};
use crate::pose::{self, Pose};

pub struct Engine {
    det: Detector,
    emb: Embedder,
    /// `None` when the MiniFASNet files are missing.
    spoof: Option<AntiSpoof>,
    /// Identifies the embedding space; profiles from another model are unusable.
    pub model_id: String,
}

#[derive(Clone, Debug)]
pub struct Analysis {
    pub face: Face,
    pub pose: Pose,
    pub features: LivenessFeatures,
    /// MiniFASNet live-face probability, on colour frames only.
    pub real_score: Option<f32>,
}

impl Engine {
    pub fn load(cfg: &ModelConfig) -> Result<Self> {
        let env = crate::ort_env()?;
        let det = Detector::load(&env, &cfg.dir.join(&cfg.detector))?;
        let emb = Embedder::load(&env, &cfg.dir.join(&cfg.recognizer), cfg.normalize)?;
        let spoof = match AntiSpoof::load(&env, &cfg.dir, &cfg.antispoof) {
            Ok(s) => Some(s),
            Err(e) => {
                warn!("photo/screen check for colour webcams unavailable: {e:#}");
                None
            }
        };
        Ok(Self { det, emb, spoof, model_id: cfg.recognizer.clone() })
    }

    pub fn has_antispoof(&self) -> bool {
        self.spoof.is_some()
    }

    /// Analyses the closest (largest) face, if any.
    pub fn analyze(&mut self, pair: &FramePair) -> Result<Option<Analysis>> {
        let faces = self.det.detect(&pair.lit)?;
        let Some(face) = faces.into_iter().max_by(|a, b| a.bbox.area().total_cmp(&b.bbox.area())) else {
            return Ok(None);
        };
        let pose = pose::estimate(&face);
        let features = liveness::measure(pair, &face);
        let real_score = match (&mut self.spoof, &pair.color) {
            (Some(s), Some(img)) => Some(s.real_score(img, &face)?),
            _ => None,
        };
        Ok(Some(Analysis { face, pose, features, real_score }))
    }

    pub fn embed(&mut self, pair: &FramePair, face: &Face) -> Result<Embedding> {
        self.emb.embed(&pair.lit, face)
    }
}
