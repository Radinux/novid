//! YuNet face detector (opencv_zoo, MIT). Anchor-free, three strides, five
//! landmarks per face: right eye, left eye, nose tip, right/left mouth corner
//! (from the subject's point of view, i.e. image-left eye first).

use std::path::Path;

use anyhow::{Context, Result};
use ndarray::Array4;
use ort::session::Session;
use ort::value::TensorRef;

use crate::image::{Gray, Rect};

#[derive(Clone, Debug)]
pub struct Face {
    pub bbox: Rect,
    pub kps: [[f32; 2]; 5],
    pub score: f32,
}

pub struct Detector {
    session: Session,
    /// Width the frame is downscaled to before inference. Faces in front of a
    /// laptop are large, so 320 is plenty and keeps this under a few ms.
    pub input_width: usize,
    pub score_threshold: f32,
    pub nms_threshold: f32,
}

const STRIDES: [usize; 3] = [8, 16, 32];

impl Detector {
    pub fn load(env: &ort::environment::Environment, path: &Path) -> Result<Self> {
        let session = crate::session(env, path).context("loading detector")?;
        Ok(Self { session, input_width: 320, score_threshold: 0.6, nms_threshold: 0.3 })
    }

    pub fn detect(&mut self, img: &Gray) -> Result<Vec<Face>> {
        let scale = self.input_width as f32 / img.w as f32;
        let sw = self.input_width;
        let sh = ((img.h as f32 * scale).round() as usize).max(1);
        let small = img.resize(sw, sh);
        // Network wants dimensions padded to a multiple of 32.
        let pw = sw.div_ceil(32) * 32;
        let ph = sh.div_ceil(32) * 32;

        let mut input = Array4::<f32>::zeros((1, 3, ph, pw));
        for y in 0..sh {
            for x in 0..sw {
                let v = small.at(x, y) as f32;
                input[[0, 0, y, x]] = v;
                input[[0, 1, y, x]] = v;
                input[[0, 2, y, x]] = v;
            }
        }

        let outputs = self.session.run(ort::inputs!["input" => TensorRef::from_array_view(&input)?])?;

        let mut faces = Vec::new();
        for s in STRIDES {
            let cols = pw / s;
            let rows = ph / s;
            let get = |name: String| -> Result<&[f32]> {
                Ok(outputs[name.as_str()].try_extract_tensor::<f32>()?.1)
            };
            let cls = get(format!("cls_{s}"))?;
            let obj = get(format!("obj_{s}"))?;
            let bbox = get(format!("bbox_{s}"))?;
            let kps = get(format!("kps_{s}"))?;
            let sf = s as f32;
            for i in 0..rows * cols {
                let score = (cls[i].clamp(0.0, 1.0) * obj[i].clamp(0.0, 1.0)).sqrt();
                if score < self.score_threshold {
                    continue;
                }
                let r = (i / cols) as f32;
                let c = (i % cols) as f32;
                let b = &bbox[i * 4..i * 4 + 4];
                let cx = (c + b[0]) * sf;
                let cy = (r + b[1]) * sf;
                let w = b[2].exp() * sf;
                let h = b[3].exp() * sf;
                let k = &kps[i * 10..i * 10 + 10];
                let mut pts = [[0f32; 2]; 5];
                for (j, p) in pts.iter_mut().enumerate() {
                    p[0] = (c + k[2 * j]) * sf / scale;
                    p[1] = (r + k[2 * j + 1]) * sf / scale;
                }
                faces.push(Face {
                    bbox: Rect { x: (cx - w / 2.0) / scale, y: (cy - h / 2.0) / scale, w: w / scale, h: h / scale },
                    kps: pts,
                    score,
                });
            }
        }
        Ok(nms(faces, self.nms_threshold))
    }
}

fn nms(mut faces: Vec<Face>, thr: f32) -> Vec<Face> {
    faces.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut keep: Vec<Face> = Vec::new();
    for f in faces {
        if keep.iter().all(|k| k.bbox.iou(&f.bbox) < thr) {
            keep.push(f);
        }
    }
    keep
}

impl Face {
    /// Distance between the eyes in pixels: our scale reference.
    pub fn iod(&self) -> f32 {
        let [a, b] = [self.kps[0], self.kps[1]];
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
    }
}
