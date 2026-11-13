//! Face embedding models. Any network taking an aligned 112×112 face and
//! returning a vector works; only the input normalisation differs.

use std::path::Path;

use anyhow::{Context, Result};
use ndarray::Array4;
use ort::session::Session;
use ort::value::TensorRef;
use serde::{Deserialize, Serialize};

use crate::align::align;
use crate::detect::Face;
use crate::image::Gray;

/// Input normalisation: `(pixel - mean) / std`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Normalize {
    pub mean: f32,
    pub std: f32,
}

impl Normalize {
    /// SFace: raw 0..255.
    pub const RAW: Self = Self { mean: 0.0, std: 1.0 };
    /// InsightFace ArcFace, AdaFace, EdgeFace: [-1, 1].
    pub const SYMMETRIC: Self = Self { mean: 127.5, std: 127.5 };
}

pub struct Embedder {
    session: Session,
    input_name: String,
    norm: Normalize,
    size: usize,
}

pub type Embedding = Vec<f32>;

impl Embedder {
    pub fn load(env: &ort::environment::Environment, path: &Path, norm: Normalize) -> Result<Self> {
        let session = crate::session(env, path).context("loading embedder")?;
        let input_name = session.inputs()[0].name().to_string();
        Ok(Self { session, input_name, norm, size: 112 })
    }

    pub fn embed(&mut self, img: &Gray, face: &Face) -> Result<Embedding> {
        let crop = align(img, face, self.size);
        self.embed_aligned(&crop)
    }

    pub fn embed_aligned(&mut self, crop: &Gray) -> Result<Embedding> {
        let n = self.size;
        let mut input = Array4::<f32>::zeros((1, 3, n, n));
        for y in 0..n {
            for x in 0..n {
                let v = (crop.at(x, y) as f32 - self.norm.mean) / self.norm.std;
                for c in 0..3 {
                    input[[0, c, y, x]] = v;
                }
            }
        }
        let out = self.session.run(ort::inputs![self.input_name.as_str() => TensorRef::from_array_view(&input)?])?;
        let (_, v) = out[0].try_extract_tensor::<f32>()?;
        Ok(l2_normalize(v))
    }
}

pub fn l2_normalize(v: &[f32]) -> Embedding {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
    v.iter().map(|x| x / n).collect()
}

/// Cosine similarity of two L2-normalised vectors.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
