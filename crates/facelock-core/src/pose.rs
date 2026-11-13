//! Coarse head pose from the five landmarks. Good enough to drive the guided
//! enrollment ("turn left a bit more") without a dedicated pose network.

use serde::{Deserialize, Serialize};

use crate::detect::Face;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Pose {
    /// Negative = subject turned to their right (face moves to image-left).
    /// Roughly degrees, calibrated on frontal ≈ 0 and ±45° ≈ ±45.
    pub yaw: f32,
    /// Negative = looking down.
    pub pitch: f32,
    /// In-plane rotation in degrees.
    pub roll: f32,
}

/// Nose-to-eyes vertical ratio on a frontal face; used as pitch zero point.
const NEUTRAL_PITCH_RATIO: f32 = 0.52;

pub fn estimate(face: &Face) -> Pose {
    let [re, le, nose, rm, lm] = face.kps;
    let eye_mid = [(re[0] + le[0]) / 2.0, (re[1] + le[1]) / 2.0];
    let mouth_mid = [(rm[0] + lm[0]) / 2.0, (rm[1] + lm[1]) / 2.0];
    let iod = face.iod().max(1.0);

    // Roll first, then measure yaw/pitch in the de-rotated frame.
    let roll = (le[1] - re[1]).atan2(le[0] - re[0]);
    let (c, s) = (roll.cos(), roll.sin());
    let rot = |p: [f32; 2]| -> [f32; 2] {
        let (x, y) = (p[0] - eye_mid[0], p[1] - eye_mid[1]);
        [c * x + s * y, -s * x + c * y]
    };
    let n = rot(nose);
    let m = rot(mouth_mid);

    // Horizontal nose offset from the eye/mouth midline, relative to the
    // inter-ocular distance. Foreshortening of the IOD itself is also a yaw cue
    // but needs the face size, which the ratio already cancels out.
    let mid_x = m[0] / 2.0;
    let yaw_ratio = (n[0] - mid_x) / iod;
    let yaw = (yaw_ratio * 2.0).clamp(-1.0, 1.0).asin().to_degrees();

    let pitch_ratio = if m[1].abs() > 1.0 { n[1] / m[1] } else { NEUTRAL_PITCH_RATIO };
    let pitch = ((NEUTRAL_PITCH_RATIO - pitch_ratio) * 3.0).clamp(-1.0, 1.0).asin().to_degrees();

    Pose { yaw, pitch, roll: roll.to_degrees() }
}
