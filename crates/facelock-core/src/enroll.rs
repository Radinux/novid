//! Guided head-sweep enrollment as a frame-driven state machine.
//!
//! Emits language-neutral codes (`turn-left`, `hold`…); front-ends translate.

use std::time::{Duration, Instant};

use anyhow::Result;

use crate::embed::Embedding;
use crate::engine::Analysis;
use crate::liveness::{self, Eyewear};
use crate::store::Sample;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Yaw,
    Pitch,
}

impl Axis {
    pub fn code(self) -> &'static str {
        match self {
            Axis::Yaw => "yaw",
            Axis::Pitch => "pitch",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hint {
    NoFace,
    More,
    Hold,
    Overshoot,
    Center,
    Straighten,
}

impl Hint {
    pub fn code(self) -> &'static str {
        match self {
            Hint::NoFace => "no-face",
            Hint::More => "more",
            Hint::Hold => "hold",
            Hint::Overshoot => "overshoot",
            Hint::Center => "center",
            Hint::Straighten => "straighten",
        }
    }
}

pub struct Step {
    pub axis: Axis,
    /// "At least this far" steps accept anything past `target - TOL`.
    pub open_ended: bool,
    /// Degrees relative to the user's frontal pose. Positive yaw = the
    /// subject turns to *their* left (camera image is not mirrored).
    pub target: f32,
    pub code: &'static str,
}

pub const STEPS: [Step; 5] = [
    Step { axis: Axis::Yaw, open_ended: false, target: 0.0, code: "look-front" },
    Step { axis: Axis::Yaw, open_ended: true, target: 15.0, code: "turn-left" },
    Step { axis: Axis::Yaw, open_ended: true, target: -15.0, code: "turn-right" },
    Step { axis: Axis::Pitch, open_ended: false, target: 12.0, code: "look-up" },
    Step { axis: Axis::Pitch, open_ended: false, target: -12.0, code: "look-down" },
];

const TOL: f32 = 5.0;
/// Pitch estimate drifts by up to ~±11° while turning left/right (measured on
/// recorded sweeps), so yaw steps tolerate more pitch than pitch steps do yaw.
const YAW_STEP_PITCH_TOL: f32 = 14.0;
const PITCH_STEP_YAW_TOL: f32 = 9.0;
const STILL_DEG: f32 = 3.0;
const MAX_YAW: f32 = 60.0;
const MAX_JUMP: f32 = 20.0;
const STEP_TIMEOUT: Duration = Duration::from_secs(12);
const CALIBRATION_FRAMES: usize = 8;
/// Fewer captured poses than this and the profile is rejected.
pub const MIN_SAMPLES: usize = 4;

#[derive(Clone, Debug)]
pub enum EnrollEvent {
    Calibrating { progress: f32 },
    Step { index: usize, total: usize, code: &'static str, axis: Axis, target: f32 },
    Progress { value: f32, off_axis: f32, hint: Hint },
    Captured { index: usize },
    Skipped { index: usize },
}

#[derive(Clone, Debug)]
pub struct EnrollOutcome {
    pub samples: Vec<Sample>,
    pub eyewear: Option<Eyewear>,
}

enum Phase {
    Calibrating(Vec<(liveness::LivenessFeatures, f32, f32)>),
    Step { index: usize, started: Option<Instant>, last: Option<(f32, f32)>, stats: StepStats, hist: Vec<(f32, f32)> },
    /// Back to the centre before step `next`, re-measuring the frontal pose
    /// (people drift after each turn).
    Recenter { next: usize, started: Option<Instant>, frames: Vec<(f32, f32)> },
    Done,
}

/// Frames needed to re-measure the frontal pose, max correction, time budget.
const RECENTER_FRAMES: usize = 4;
const RECENTER_TOL: f32 = 10.0;
const RECENTER_TIMEOUT: Duration = Duration::from_secs(6);

fn step_phase(index: usize) -> Phase {
    Phase::Step { index, started: None, last: None, stats: StepStats::default(), hist: Vec::new() }
}

pub struct Enroller {
    phase: Phase,
    base: (f32, f32),
    eyewear: Option<Eyewear>,
    samples: Vec<Sample>,
}

impl Default for Enroller {
    fn default() -> Self {
        Self::new()
    }
}

impl Enroller {
    pub fn new() -> Self {
        Self { phase: Phase::Calibrating(Vec::new()), base: (0.0, 0.0), eyewear: None, samples: Vec::new() }
    }

    pub fn is_done(&self) -> bool {
        matches!(self.phase, Phase::Done)
    }

    pub fn outcome(self) -> Option<EnrollOutcome> {
        (self.samples.len() >= MIN_SAMPLES).then_some(EnrollOutcome { samples: self.samples, eyewear: self.eyewear })
    }

    /// Feeds one analysed frame. `embed` is only called when a pose is captured.
    pub fn feed(
        &mut self,
        now: Instant,
        a: Option<&Analysis>,
        embed: &mut dyn FnMut() -> Result<Embedding>,
    ) -> Result<Vec<EnrollEvent>> {
        let mut ev = Vec::new();
        // Turned faces score lower; 0.8 lost half the frames past ~25°.
        let usable = a.filter(|a| a.face.score > 0.65 && a.face.iod() > 25.0);

        match &mut self.phase {
            Phase::Done => {}
            Phase::Recenter { next, started, frames } => {
                let t0 = *started.get_or_insert_with(|| {
                    ev.push(EnrollEvent::Step {
                        index: *next,
                        total: STEPS.len(),
                        code: "recenter",
                        axis: Axis::Yaw,
                        target: 0.0,
                    });
                    now
                });
                let next = *next;
                if let Some(a) = usable {
                    let rel = (a.pose.yaw - self.base.0, a.pose.pitch - self.base.1);
                    let centred = rel.0.abs() < RECENTER_TOL && rel.1.abs() < RECENTER_TOL;
                    // Must be still too: compare with the previous accepted frame.
                    let still = frames.last().is_none_or(|(y, p)| (a.pose.yaw - y).abs() < STILL_DEG && (a.pose.pitch - p).abs() < STILL_DEG * 2.0);
                    if centred && still {
                        frames.push((a.pose.yaw, a.pose.pitch));
                    } else {
                        frames.clear();
                    }
                    let h = if centred { Hint::Hold } else { Hint::Center };
                    ev.push(EnrollEvent::Progress { value: rel.0, off_axis: rel.1, hint: h });
                } else {
                    frames.clear();
                    ev.push(EnrollEvent::Progress { value: 0.0, off_axis: 0.0, hint: Hint::NoFace });
                }
                if frames.len() >= RECENTER_FRAMES {
                    let med = |mut v: Vec<f32>| {
                        v.sort_by(f32::total_cmp);
                        v[v.len() / 2]
                    };
                    let nb = (med(frames.iter().map(|f| f.0).collect()), med(frames.iter().map(|f| f.1).collect()));
                    log::info!(
                        "recentred before {}: reference moved by {:+.1}° yaw, {:+.1}° pitch",
                        STEPS[next].code,
                        nb.0 - self.base.0,
                        nb.1 - self.base.1
                    );
                    self.base = nb;
                    self.phase = step_phase(next);
                } else if now.duration_since(t0) > RECENTER_TIMEOUT {
                    log::info!("recentre before {} timed out, keeping reference", STEPS[next].code);
                    self.phase = step_phase(next);
                }
            }
            Phase::Calibrating(frames) => {
                if let Some(a) = usable.filter(|a| a.pose.yaw.abs() < 12.0) {
                    frames.push((a.features, a.pose.yaw, a.pose.pitch));
                }
                ev.push(EnrollEvent::Calibrating { progress: frames.len() as f32 / CALIBRATION_FRAMES as f32 });
                if frames.len() >= CALIBRATION_FRAMES {
                    let med = |mut v: Vec<f32>| {
                        v.sort_by(f32::total_cmp);
                        v[v.len() / 2]
                    };
                    self.base = (med(frames.iter().map(|f| f.1).collect()), med(frames.iter().map(|f| f.2).collect()));
                    let rel: Vec<_> = frames.iter().map(|(f, y, p)| (*f, y - self.base.0, p - self.base.1)).collect();
                    self.eyewear = liveness::classify_eyewear(&rel);
                    self.phase = step_phase(0);
                }
            }
            Phase::Step { index, started, last, stats, hist } => {
                let step = &STEPS[*index];
                let started_at = *started.get_or_insert_with(|| {
                    ev.push(EnrollEvent::Step {
                        index: *index,
                        total: STEPS.len(),
                        code: step.code,
                        axis: step.axis,
                        target: step.target,
                    });
                    now
                });
                let mut advance = None;
                if now.duration_since(started_at) > STEP_TIMEOUT {
                    advance = Some(EnrollEvent::Skipped { index: *index });
                } else if let Some(a) = usable {
                    let raw = (a.pose.yaw - self.base.0, a.pose.pitch - self.base.1);
                    let med = |h: &[(f32, f32)], f: fn(&(f32, f32)) -> f32| {
                        let mut v: Vec<f32> = h.iter().map(f).collect();
                        v.sort_by(f32::total_cmp);
                        v[v.len() / 2]
                    };
                    // Landmarks jitter on turned faces (the pitch estimate
                    // jumped ±9° frame to frame on right turns): drop
                    // implausible poses, then use a 3-frame median.
                    let leap = !hist.is_empty() && (raw.0 - med(hist, |p| p.0)).abs() >= MAX_JUMP;
                    if raw.0.abs() >= MAX_YAW || leap {
                        stats.outliers += 1;
                        if leap && stats.outliers % 3 == 0 {
                            hist.clear(); // a genuine fast move: start over from here
                        }
                        return Ok(ev);
                    }
                    if hist.len() == 3 {
                        hist.remove(0);
                    }
                    hist.push(raw);
                    let (yaw, pitch) = (med(hist, |p| p.0), med(hist, |p| p.1));
                    let (value, off) = match step.axis {
                        Axis::Yaw => (yaw, pitch),
                        Axis::Pitch => (pitch, yaw),
                    };
                    // Stillness is judged on the step's own axis only.
                    let still = hist.len() == 3 && last.is_some_and(|(lv, _)| (value - lv).abs() < STILL_DEG);
                    *last = Some((value, off));
                    let on_target = step.reached(value) && off.abs() < step.off_axis_tol();
                    let h = hint(step, value, off, on_target);
                    stats.record(value, h);
                    ev.push(EnrollEvent::Progress { value, off_axis: off, hint: h });
                    if on_target && still {
                        self.samples.push(Sample { yaw, pitch, embedding: embed()? });
                        advance = Some(EnrollEvent::Captured { index: *index });
                    }
                } else {
                    *last = None;
                    hist.clear();
                    stats.record(f32::NAN, Hint::NoFace);
                    ev.push(EnrollEvent::Progress { value: 0.0, off_axis: 0.0, hint: Hint::NoFace });
                }
                if let Some(e) = advance {
                    log::info!(
                        "enroll step {} {}: {} after {:.1}s, {}",
                        *index + 1,
                        step.code,
                        if matches!(e, EnrollEvent::Captured { .. }) { "captured" } else { "SKIPPED" },
                        now.duration_since(started_at).as_secs_f32(),
                        stats.summary()
                    );
                    ev.push(e);
                    if *index + 1 < STEPS.len() {
                        // Straight from the frontal step into "left" needs no
                        // recentre; every later direction starts from centre.
                        self.phase = if *index == 0 { step_phase(1) } else { Phase::Recenter { next: *index + 1, started: None, frames: Vec::new() } };
                    } else {
                        self.phase = Phase::Done;
                    }
                }
            }
        }
        Ok(ev)
    }
}

impl Step {
    fn reached(&self, value: f32) -> bool {
        if self.open_ended {
            value.signum() == self.target.signum() && value.abs() > self.target.abs() - TOL && value.abs() < 50.0
        } else {
            (value - self.target).abs() < TOL
        }
    }

    fn off_axis_tol(&self) -> f32 {
        match self.axis {
            Axis::Yaw => YAW_STEP_PITCH_TOL,
            Axis::Pitch => PITCH_STEP_YAW_TOL,
        }
    }
}

/// Per-step telemetry for the journal: why did a step take long or fail?
#[derive(Default)]
struct StepStats {
    frames: u32,
    outliers: u32,
    best: f32,
    hints: std::collections::BTreeMap<&'static str, u32>,
}

impl StepStats {
    fn record(&mut self, value: f32, h: Hint) {
        self.frames += 1;
        if value.is_finite() && value.abs() > self.best.abs() {
            self.best = value;
        }
        *self.hints.entry(h.code()).or_default() += 1;
    }

    fn summary(&self) -> String {
        let hints: Vec<String> = self.hints.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!(
            "{} frames ({} outliers dropped), furthest {:+.1}°, hints [{}]",
            self.frames + self.outliers,
            self.outliers,
            self.best,
            hints.join(" ")
        )
    }
}

fn hint(step: &Step, value: f32, off: f32, on_target: bool) -> Hint {
    if off.abs() > step.off_axis_tol() {
        return Hint::Straighten;
    }
    if on_target {
        return Hint::Hold;
    }
    // Still on the previous step's side (e.g. coming back from the far
    // left to go right): guide back through the centre, don't scold.
    if step.target != 0.0 && value.signum() != step.target.signum() && value.abs() > 8.0 {
        return Hint::Center;
    }
    if value.signum() == step.target.signum() && value.abs() > step.target.abs() + TOL && !step.open_ended {
        return Hint::Overshoot;
    }
    if step.target == 0.0 { Hint::Center } else { Hint::More }
}
