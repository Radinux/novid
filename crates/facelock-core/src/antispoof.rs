//! Photo / screen check for colour webcams: MiniFASNet from Minivision's
//! Silent-Face-Anti-Spoofing (Apache-2.0), ONNX export by yakhyo.
//!
//! Two networks look at the face with different amounts of surroundings
//! (crop 2.7× and 4× the face box, as they were trained) and their softmax
//! outputs are averaged. Class 1 is a live face; the others are print and
//! replay (screen) attacks.
//!
//! The models were trained on colour images, so they are not used on IR
//! frames, where the strobed emitter is a stronger physical check anyway.

use std::path::Path;

use anyhow::{Context, Result};
use ndarray::Array4;
use ort::session::Session;
use ort::value::TensorRef;
use serde::{Deserialize, Serialize};

use crate::detect::Face;
use crate::image::{Bgr, Rect};

const INPUT: usize = 80;
const REAL: usize = 1;

/// One MiniFASNet network and the crop it expects.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpoofModel {
    pub file: String,
    /// Crop size relative to the face box.
    pub scale: f32,
}

pub struct AntiSpoof {
    nets: Vec<(Session, String, f32)>,
}

impl AntiSpoof {
    pub fn load(env: &ort::environment::Environment, dir: &Path, models: &[SpoofModel]) -> Result<Self> {
        let mut nets = Vec::new();
        for m in models {
            let session = crate::session(env, &dir.join(&m.file)).with_context(|| format!("loading {}", m.file))?;
            let input = session.inputs()[0].name().to_string();
            nets.push((session, input, m.scale));
        }
        Ok(Self { nets })
    }

    /// Probability, from 0 to 1, that `face` is a live person rather than a
    /// photo or a screen.
    pub fn real_score(&mut self, img: &Bgr, face: &Face) -> Result<f32> {
        let mut sum = 0.0;
        for (session, input_name, scale) in &mut self.nets {
            let crop = img.crop_resize(crop_box(img.w, img.h, &face.bbox, *scale), INPUT, INPUT);
            let mut input = Array4::<f32>::zeros((1, 3, INPUT, INPUT));
            for y in 0..INPUT {
                for x in 0..INPUT {
                    for c in 0..3 {
                        input[[0, c, y, x]] = crop.px[(y * INPUT + x) * 3 + c] as f32;
                    }
                }
            }
            let out = session.run(ort::inputs![input_name.as_str() => TensorRef::from_array_view(&input)?])?;
            let (_, logits) = out[0].try_extract_tensor::<f32>()?;
            sum += softmax(logits)[REAL];
        }
        Ok(sum / self.nets.len().max(1) as f32)
    }
}

fn softmax(v: &[f32]) -> Vec<f32> {
    let m = v.iter().copied().fold(f32::MIN, f32::max);
    let e: Vec<f32> = v.iter().map(|x| (x - m).exp()).collect();
    let s: f32 = e.iter().sum();
    e.into_iter().map(|x| x / s).collect()
}

/// The training crop: the face box scaled around its centre (less when the
/// frame is too small), shifted back inside the frame rather than clipped so
/// the aspect ratio is kept. Inclusive pixel bounds.
fn crop_box(src_w: usize, src_h: usize, b: &Rect, scale: f32) -> (usize, usize, usize, usize) {
    let (w, h) = (b.w.max(1.0), b.h.max(1.0));
    let (sw, sh) = (src_w as f32 - 1.0, src_h as f32 - 1.0);
    let scale = scale.min(sh / h).min(sw / w);
    let (nw, nh) = (w * scale, h * scale);
    let (cx, cy) = (b.x + w / 2.0, b.y + h / 2.0);
    let (mut x0, mut y0, mut x1, mut y1) = (cx - nw / 2.0, cy - nh / 2.0, cx + nw / 2.0, cy + nh / 2.0);
    if x0 < 0.0 {
        x1 -= x0;
        x0 = 0.0;
    }
    if y0 < 0.0 {
        y1 -= y0;
        y0 = 0.0;
    }
    if x1 > sw {
        x0 -= x1 - sw;
        x1 = sw;
    }
    if y1 > sh {
        y0 -= y1 - sh;
        y1 = sh;
    }
    (x0.max(0.0) as usize, y0.max(0.0) as usize, x1 as usize, y1 as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_is_shifted_inside_the_frame() {
        // Face near the left edge: the 2.7× box is moved right, not clipped.
        let b = Rect { x: 10.0, y: 100.0, w: 100.0, h: 100.0 };
        let (x0, y0, x1, y1) = crop_box(640, 360, &b, 2.7);
        assert_eq!(x0, 0);
        assert_eq!(x1, 270);
        assert_eq!((y0, y1), (15, 285));
    }

    #[test]
    fn crop_scale_is_limited_by_the_frame() {
        // 4× a 150 px face does not fit in 360 px: scale drops to 359/150.
        let b = Rect { x: 245.0, y: 105.0, w: 150.0, h: 150.0 };
        let (_, y0, _, y1) = crop_box(640, 360, &b, 4.0);
        assert_eq!((y0, y1), (0, 359));
    }

    #[test]
    fn softmax_sums_to_one() {
        let p = softmax(&[1.0, 3.0, -2.0]);
        assert!((p.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert!(p[1] > p[0] && p[0] > p[2]);
    }
}
