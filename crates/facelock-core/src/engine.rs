//! Models + per-frame analysis, shared by the daemon and the CLI.

use anyhow::Result;

use crate::camera::FramePair;
use crate::config::ModelConfig;
use crate::detect::{Detector, Face};
use crate::embed::{Embedder, Embedding};
use crate::liveness::{self, LivenessFeatures};
use crate::pose::{self, Pose};

pub struct Engine {
    det: Detector,
    emb: Embedder,
    /// Identifies the embedding space; profiles from another model are unusable.
    pub model_id: String,
}

#[derive(Clone, Debug)]
pub struct Analysis {
    pub face: Face,
    pub pose: Pose,
    pub features: LivenessFeatures,
}

impl Engine {
    pub fn load(cfg: &ModelConfig) -> Result<Self> {
        let env = crate::ort_env()?;
        let det = Detector::load(&env, &cfg.dir.join(&cfg.detector))?;
        let emb = Embedder::load(&env, &cfg.dir.join(&cfg.recognizer), cfg.normalize)?;
        Ok(Self { det, emb, model_id: cfg.recognizer.clone() })
    }

    /// Analyses the closest (largest) face, if any.
    pub fn analyze(&mut self, pair: &FramePair) -> Result<Option<Analysis>> {
        let faces = self.det.detect(&pair.lit)?;
        let Some(face) = faces.into_iter().max_by(|a, b| a.bbox.area().total_cmp(&b.bbox.area())) else {
            return Ok(None);
        };
        let pose = pose::estimate(&face);
        let features = liveness::measure(pair, &face);
        Ok(Some(Analysis { face, pose, features }))
    }

    pub fn embed(&mut self, pair: &FramePair, face: &Face) -> Result<Embedding> {
        self.emb.embed(&pair.lit, face)
    }
}
