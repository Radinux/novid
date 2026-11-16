//! Authentication decision over a short burst of frames.

use crate::config::VerifyConfig;
use crate::embed::{Embedding, cosine};
use crate::engine::Analysis;
use crate::liveness::{self, LivenessFeatures};
use crate::store::Profile;

#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    Match { profile_id: String, similarity: f32 },
    /// Language-neutral reason code.
    Reject(&'static str),
}

/// Strong matches that settle a verification early (see `fast_threshold`).
const FAST_MATCHES: u32 = 2;

pub struct Verifier<'a> {
    cfg: &'a VerifyConfig,
    profiles: Vec<&'a Profile>,
    /// (profile index, similarity) of frames above threshold.
    matches: Vec<(usize, f32)>,
    obs: Vec<(LivenessFeatures, f32, f32)>,
    frames: u32,
    faces: u32,
    no_strobe: u32,
    low_ir: u32,
    spoof: u32,
    best_sim: f32,
}

impl<'a> Verifier<'a> {
    /// Only profiles built with `model_id` are comparable.
    pub fn new(cfg: &'a VerifyConfig, profiles: &'a [Profile], model_id: &str) -> Self {
        Self {
            cfg,
            profiles: profiles.iter().filter(|p| p.model == model_id).collect(),
            matches: Vec::new(),
            obs: Vec::new(),
            frames: 0,
            faces: 0,
            no_strobe: 0,
            low_ir: 0,
            spoof: 0,
            best_sim: -1.0,
        }
    }

    pub fn has_profiles(&self) -> bool {
        !self.profiles.is_empty()
    }

    /// Whether this frame deserves an embedding (saves work on rejected frames).
    pub fn wants_embedding(&mut self, a: Option<&Analysis>) -> bool {
        self.frames += 1;
        let Some(a) = a else { return false };
        self.faces += 1;
        if self.cfg.rgb_antispoof
            && let Some(s) = a.real_score
            && s < self.cfg.min_real_score
        {
            self.spoof += 1;
            return false;
        }
        match a.features.emitter_gain {
            None if self.cfg.require_strobe => {
                self.no_strobe += 1;
                false
            }
            Some(g) if g < self.cfg.min_emitter_gain => {
                self.low_ir += 1;
                false
            }
            _ => true,
        }
    }

    /// Feeds an accepted frame. Returns a verdict as soon as one is certain.
    pub fn feed(&mut self, a: &Analysis, emb: &Embedding) -> Option<Verdict> {
        self.obs.push((a.features, a.pose.yaw, a.pose.pitch));
        let best = self
            .profiles
            .iter()
            .enumerate()
            .flat_map(|(i, p)| p.samples.iter().map(move |s| (i, cosine(emb, &s.embedding))))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, sim)) = best {
            self.best_sim = self.best_sim.max(sim);
            if sim >= self.cfg.threshold {
                self.matches.push((i, sim));
            }
        }
        if !self.enough_matches() {
            return None;
        }
        // Profile with the most matching frames wins.
        let mut counts = vec![0u32; self.profiles.len()];
        for (i, _) in &self.matches {
            counts[*i] += 1;
        }
        let winner = (0..counts.len()).max_by_key(|i| counts[*i])?;
        let sim = self.matches.iter().filter(|(i, _)| *i == winner).map(|m| m.1).fold(0.0, f32::max);
        let profile = self.profiles[winner];

        if self.cfg.eyewear_check
            && let Some(expected) = profile.eyewear
        {
            match liveness::classify_eyewear(&self.obs) {
                None => return None, // need more frontal frames
                Some(seen) if seen != expected => {
                    // Another matching profile may have been enrolled with the
                    // other eyewear (e.g. one with glasses, one without).
                    let ok = self.matches.iter().any(|(i, _)| self.profiles[*i].eyewear == Some(seen));
                    if !ok {
                        return Some(Verdict::Reject("eyewear-mismatch"));
                    }
                }
                Some(_) => {}
            }
        }
        Some(Verdict::Match { profile_id: profile.id.clone(), similarity: sim })
    }

    /// `min_matches` frames, or two clear ones: a lit frame only comes every
    /// other camera frame, so each one saved is ~130 ms on a 15 fps IR camera.
    fn enough_matches(&self) -> bool {
        let n = self.matches.len() as u32;
        let strong = self.matches.iter().filter(|m| m.1 >= self.cfg.fast_threshold).count() as u32;
        n >= self.cfg.min_matches || strong >= FAST_MATCHES.min(self.cfg.min_matches)
    }

    /// Verdict when the time budget runs out.
    pub fn finish(&self) -> Verdict {
        if self.profiles.is_empty() {
            Verdict::Reject("no-profile")
        } else if self.faces == 0 {
            Verdict::Reject("no-face")
        } else if self.spoof * 2 > self.faces {
            Verdict::Reject("spoof")
        } else if self.no_strobe * 2 > self.faces {
            Verdict::Reject("no-ir-strobe")
        } else if self.low_ir * 2 > self.faces {
            Verdict::Reject("low-ir")
        } else if self.matches.len() as u32 >= self.cfg.min_matches {
            Verdict::Reject("eyewear-undecided")
        } else {
            Verdict::Reject("no-match")
        }
    }

    pub fn best_similarity(&self) -> f32 {
        self.best_sim
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Face;
    use crate::image::Rect;
    use crate::pose::Pose;
    use crate::store::Sample;

    fn profile() -> Profile {
        let mut e = vec![0.0; 128];
        e[0] = 1.0;
        Profile {
            id: "p".into(),
            label: String::new(),
            created: 0,
            model: "m".into(),
            eyewear: None,
            samples: vec![Sample { yaw: 0.0, pitch: 0.0, embedding: e }],
        }
    }

    /// Unit embedding whose cosine with the profile sample is `sim`.
    fn frame(sim: f32) -> (Analysis, Embedding) {
        let mut e = vec![0.0; 128];
        e[0] = sim;
        e[1] = (1.0 - sim * sim).sqrt();
        let face = Face { bbox: Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 }, kps: [[0.0; 2]; 5], score: 1.0 };
        let features = LivenessFeatures { emitter_gain: Some(100.0), ..Default::default() };
        (Analysis { face, pose: Pose::default(), features, real_score: None }, e)
    }

    /// Frames fed before a verdict, and the verdict.
    fn run(cfg: &VerifyConfig, sims: &[f32]) -> (usize, Option<Verdict>) {
        let profiles = [profile()];
        let mut v = Verifier::new(cfg, &profiles, "m");
        for (i, s) in sims.iter().enumerate() {
            let (a, e) = frame(*s);
            assert!(v.wants_embedding(Some(&a)));
            if let Some(verdict) = v.feed(&a, &e) {
                return (i + 1, Some(verdict));
            }
        }
        (sims.len(), None)
    }

    #[test]
    fn colour_photo_is_refused() {
        let cfg = VerifyConfig { require_strobe: false, ..VerifyConfig::default() };
        let profiles = [profile()];
        let mut v = Verifier::new(&cfg, &profiles, "m");
        for score in [0.1, 0.2, 0.9, 0.1] {
            let (mut a, e) = frame(0.95);
            a.features.emitter_gain = None;
            a.real_score = Some(score);
            if v.wants_embedding(Some(&a)) {
                // A single frame taken for live is not enough to match.
                assert_eq!(v.feed(&a, &e), None);
            }
        }
        assert_eq!(v.finish(), Verdict::Reject("spoof"));
    }

    #[test]
    fn colour_live_face_matches() {
        let cfg = VerifyConfig { require_strobe: false, ..VerifyConfig::default() };
        let profiles = [profile()];
        let mut v = Verifier::new(&cfg, &profiles, "m");
        let (mut a, e) = frame(0.9);
        a.features.emitter_gain = None;
        a.real_score = Some(0.95);
        assert!(v.wants_embedding(Some(&a)));
        assert_eq!(v.feed(&a, &e), None);
        assert!(v.wants_embedding(Some(&a)));
        assert!(matches!(v.feed(&a, &e), Some(Verdict::Match { .. })));
    }

    #[test]
    fn two_clear_frames_are_enough() {
        let (n, v) = run(&VerifyConfig::default(), &[0.9, 0.88, 0.9]);
        assert_eq!(n, 2);
        assert!(matches!(v, Some(Verdict::Match { .. })));
    }

    #[test]
    fn borderline_frames_need_min_matches() {
        let (n, v) = run(&VerifyConfig::default(), &[0.75, 0.9, 0.75, 0.9]);
        assert_eq!(n, 3);
        assert!(matches!(v, Some(Verdict::Match { .. })));
    }

    #[test]
    fn shortcut_can_be_turned_off() {
        let cfg = VerifyConfig { fast_threshold: 1.0, ..Default::default() };
        assert_eq!(run(&cfg, &[0.9, 0.9, 0.9]).0, 3);
    }

    #[test]
    fn no_match_below_threshold() {
        assert_eq!(run(&VerifyConfig::default(), &[0.3, 0.4, 0.2]).1, None);
    }
}
