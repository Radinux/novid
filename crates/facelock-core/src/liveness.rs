//! Presentation-attack cues available from a strobed IR camera.
//!
//! These are measurements, not a verdict: thresholds are calibrated on real
//! captures (live face vs. phone screen vs. printed photo) before being used
//! to gate authentication.
//!
//! * `emitter_gain` – how much the face brightens when the IR LED fires.
//!   Phone/laptop screens barely reflect NIR diffusely and look almost black.
//! * `glint` – corneal specular highlight of the emitter in each eye: a sharp
//!   peak present in the lit frame and absent from the dark one. Printed eyes
//!   are diffuse and cannot exceed the brightness of surrounding "skin".
//! * `sharpness` – Laplacian energy of the face; recaptured media lose detail.

use serde::{Deserialize, Serialize};

use crate::camera::FramePair;
use crate::detect::Face;
use crate::image::{Gray, Rect};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct LivenessFeatures {
    /// Mean(lit − dark) over the face, in 0..255. `None` without strobing.
    pub emitter_gain: Option<f32>,
    /// Mean of the lit face, 0..255.
    pub lit_level: f32,
    /// Weakest of the two eye glints: peak minus local median of the
    /// emitter-only image, divided by the face's emitter gain.
    pub glint: Option<f32>,
    /// Mean area of the eye highlights in units of IOD² (×1000). Corneal
    /// glints are tiny; lens reflections are larger blobs.
    pub glint_area: Option<f32>,
    /// Mean distance of the highlight centroid from the eye landmark, / IOD.
    /// Corneal glints sit on the pupil; lens reflections drift off-centre.
    pub glint_offset: Option<f32>,
    pub sharpness: f32,
}

/// What the eye reflections say about spectacles, decided over several
/// near-frontal frames (single frames are too noisy).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Eyewear {
    None,
    Glasses,
}

/// Calibrated on recorded sessions (frontal medians): bare eyes gave
/// offset ≈ 0.06 / area ≈ 2.2, glasses offset ≈ 0.115 / area ≈ 6.3.
/// Per-session values overlap, so only clear-cut medians yield a verdict;
/// everything in between is "undecided", which never causes a rejection.
const GLASSES_OFFSET: f32 = 0.10;
const GLASSES_AREA: f32 = 4.5;
const BARE_OFFSET: f32 = 0.075;
const BARE_AREA: f32 = 3.0;
const MIN_FRAMES: usize = 5;

/// Votes over `(features, yaw, pitch)` samples; `None` if too few usable
/// frontal frames or if the reflections are ambiguous.
pub fn classify_eyewear(samples: &[(LivenessFeatures, f32, f32)]) -> Option<Eyewear> {
    let frontal: Vec<&LivenessFeatures> =
        samples.iter().filter(|(_, y, p)| y.abs() < 10.0 && p.abs() < 10.0).map(|(f, _, _)| f).collect();
    let mut off: Vec<f32> = frontal.iter().filter_map(|f| f.glint_offset).collect();
    let mut area: Vec<f32> = frontal.iter().filter_map(|f| f.glint_area).collect();
    if off.len() < MIN_FRAMES {
        return None;
    }
    let med = |v: &mut Vec<f32>| {
        v.sort_by(f32::total_cmp);
        v[v.len() / 2]
    };
    let (o, a) = (med(&mut off), med(&mut area));
    if o > GLASSES_OFFSET && a > GLASSES_AREA {
        Some(Eyewear::Glasses)
    } else if o < BARE_OFFSET && a < BARE_AREA {
        Some(Eyewear::None)
    } else {
        None
    }
}

pub fn measure(pair: &FramePair, face: &Face) -> LivenessFeatures {
    let inner = inner_face(face);
    let lit_level = pair.lit.mean_in(&inner);
    let sharpness = laplacian_energy(&pair.lit, &inner);
    let mut f = LivenessFeatures { lit_level, sharpness, ..Default::default() };

    let Some(diff) = pair.emitter_only() else {
        return f;
    };
    let gain = diff.mean_in(&inner);
    let iod = face.iod().max(1.0);
    let r = (iod * 0.16).max(3.0);
    let g0 = glint_strength(&diff, face.kps[0], r);
    let g1 = glint_strength(&diff, face.kps[1], r);
    f.emitter_gain = Some(gain);
    f.glint = Some(g0.min(g1) / gain.max(1.0));

    let b0 = highlight_blob(&diff, face.kps[0], r);
    let b1 = highlight_blob(&diff, face.kps[1], r);
    f.glint_area = Some((b0.0 + b1.0) / 2.0 / (iod * iod) * 1000.0);
    f.glint_offset = Some((b0.1 + b1.1) / 2.0 / iod);
    f
}

/// Brightest connected highlight in the eye window of the emitter-only image.
/// Returns (area in px², centroid distance from the eye landmark in px).
fn highlight_blob(diff: &Gray, eye: [f32; 2], r: f32) -> (f32, f32) {
    let win = Rect { x: eye[0] - r, y: eye[1] - r, w: 2.0 * r, h: 2.0 * r };
    let (x0, y0, x1, y1) = win.clip(diff.w, diff.h);
    let (ww, wh) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
    if ww == 0 || wh == 0 {
        return (0.0, 0.0);
    }
    let mut vals: Vec<u8> = (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).map(|(x, y)| diff.at(x, y)).collect();
    vals.sort_unstable();
    let median = vals[vals.len() / 2] as f32;
    let peak = *vals.last().unwrap() as f32;
    let thr = median + (peak - median) * 0.5;
    if peak - median < 8.0 {
        return (0.0, 0.0);
    }
    // Flood fill from the brightest pixel.
    let (mut sx, mut sy, mut best) = (x0, y0, 0u8);
    for y in y0..y1 {
        for x in x0..x1 {
            if diff.at(x, y) > best {
                (sx, sy, best) = (x, y, diff.at(x, y));
            }
        }
    }
    let mut seen = vec![false; ww * wh];
    let mut stack = vec![(sx, sy)];
    let (mut n, mut cx, mut cy) = (0f32, 0f32, 0f32);
    while let Some((x, y)) = stack.pop() {
        let i = (y - y0) * ww + (x - x0);
        if seen[i] || (diff.at(x, y) as f32) < thr {
            continue;
        }
        seen[i] = true;
        n += 1.0;
        cx += x as f32;
        cy += y as f32;
        if x > x0 { stack.push((x - 1, y)); }
        if x + 1 < x1 { stack.push((x + 1, y)); }
        if y > y0 { stack.push((x, y - 1)); }
        if y + 1 < y1 { stack.push((x, y + 1)); }
    }
    let (cx, cy) = (cx / n, cy / n);
    (n, ((cx - eye[0]).powi(2) + (cy - eye[1]).powi(2)).sqrt())
}

/// Central part of the face box (skips hair/background at the edges).
fn inner_face(face: &Face) -> Rect {
    let b = face.bbox;
    Rect { x: b.x + b.w * 0.2, y: b.y + b.h * 0.2, w: b.w * 0.6, h: b.h * 0.6 }
}

fn glint_strength(diff: &Gray, eye: [f32; 2], r: f32) -> f32 {
    let win = Rect { x: eye[0] - r, y: eye[1] - r, w: 2.0 * r, h: 2.0 * r };
    let (x0, y0, x1, y1) = win.clip(diff.w, diff.h);
    let mut vals: Vec<u8> = Vec::with_capacity((x1 - x0) * (y1 - y0));
    for y in y0..y1 {
        for x in x0..x1 {
            vals.push(diff.at(x, y));
        }
    }
    if vals.is_empty() {
        return 0.0;
    }
    vals.sort_unstable();
    let median = vals[vals.len() / 2] as f32;
    // Use the 98th percentile rather than the max to ignore single hot pixels.
    let peak = vals[(vals.len() * 98 / 100).min(vals.len() - 1)] as f32;
    peak - median
}

fn laplacian_energy(img: &Gray, r: &Rect) -> f32 {
    let (x0, y0, x1, y1) = r.clip(img.w, img.h);
    if x1 < x0 + 3 || y1 < y0 + 3 {
        return 0.0;
    }
    let (mut s, mut n) = (0f64, 0u64);
    for y in y0 + 1..y1 - 1 {
        for x in x0 + 1..x1 - 1 {
            let c = img.at(x, y) as i32 * 4;
            let l = img.at(x - 1, y) as i32
                + img.at(x + 1, y) as i32
                + img.at(x, y - 1) as i32
                + img.at(x, y + 1) as i32;
            let v = (c - l) as f64;
            s += v * v;
            n += 1;
        }
    }
    (s / n as f64).sqrt() as f32
}
