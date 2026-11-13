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
        if (self.matches.len() as u32) < self.cfg.min_matches {
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

    /// Verdict when the time budget runs out.
    pub fn finish(&self) -> Verdict {
        if self.profiles.is_empty() {
            Verdict::Reject("no-profile")
        } else if self.faces == 0 {
            Verdict::Reject("no-face")
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
