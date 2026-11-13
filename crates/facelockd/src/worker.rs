//! The single thread that owns the models and, during a job, the camera.
//! Jobs run one at a time, which serialises camera access for free.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use facelock_core::camera::{self, CameraInfo, CameraKind};
use facelock_core::config::Config;
use facelock_core::engine::{Analysis, Engine};
use facelock_core::enroll::{EnrollEvent, EnrollOutcome, Enroller};
use facelock_core::feed::CameraFeed;
use facelock_core::liveness::{self, Eyewear};
use facelock_core::store::Profile;
use facelock_core::verify::{Verdict, Verifier};
use log::{error, info, warn};
use tokio::sync::{mpsc, oneshot};

const ENROLL_TIMEOUT: Duration = Duration::from_secs(120);
const PREVIEW_W: usize = 320;
const PREVIEW_H: usize = 180;

pub enum Job {
    Verify { user: String, profiles: Vec<Profile>, reply: oneshot::Sender<Verdict> },
    Enroll { events: mpsc::UnboundedSender<Event>, cancel: Arc<AtomicBool>, reply: oneshot::Sender<Result<EnrollOutcome, String>> },
    Preview { profiles: Vec<Profile>, events: mpsc::UnboundedSender<Event>, cancel: Arc<AtomicBool> },
}

pub enum Event {
    /// Downscaled lit frame; `face` = bbox (x, y, w, h) then 5 landmarks, in frame pixels.
    Frame { width: u32, height: u32, pixels: Vec<u8>, face: Vec<f64> },
    Enroll(EnrollEvent),
    Stats { similarity: f64, emitter_gain: f64, yaw: f64, pitch: f64, eyewear: &'static str },
}

/// Camera in use, shared with the D-Bus side (properties). `None` if no
/// usable camera was found; detection is retried when a job needs one.
pub type SharedCamera = Arc<std::sync::Mutex<Option<CameraInfo>>>;

fn detect(cfg: &Config, shared: &SharedCamera) -> Option<CameraInfo> {
    match camera::detect(&cfg.camera) {
        Ok(info) => {
            info!(
                "camera: {} ({}, {}x{}) at {}",
                info.name,
                info.kind.code(),
                info.width,
                info.height,
                info.path().display()
            );
            if info.kind == CameraKind::Rgb {
                warn!("colour camera in use: no IR liveness, photos may pass; keyring release disabled");
            }
            *shared.lock().unwrap() = Some(info.clone());
            Some(info)
        }
        Err(e) => {
            warn!("camera detection: {e:#}");
            *shared.lock().unwrap() = None;
            None
        }
    }
}

/// Opens the camera, re-detecting once if the known device is gone or broken
/// (e.g. /dev/videoN renumbered after a USB change).
fn open_feed(cfg: &Config, shared: &SharedCamera) -> Result<(CameraFeed, CameraInfo)> {
    let known = shared.lock().unwrap().clone();
    if let Some(info) = known
        && let Ok(f) = CameraFeed::start(&info, cfg.camera.fps)
    {
        return Ok((f, info));
    }
    let info = detect(cfg, shared).ok_or_else(|| anyhow::anyhow!("no usable camera"))?;
    Ok((CameraFeed::start(&info, cfg.camera.fps)?, info))
}

pub fn spawn(cfg: Arc<Config>, shared: SharedCamera) -> Result<(std::sync::mpsc::Sender<Job>, String)> {
    let (tx, rx) = std::sync::mpsc::channel::<Job>();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<String, String>>(1);
    std::thread::Builder::new().name("worker".into()).spawn(move || {
        let mut engine = match Engine::load(&cfg.models) {
            Ok(e) => {
                let _ = ready_tx.send(Ok(e.model_id.clone()));
                e
            }
            Err(e) => {
                let _ = ready_tx.send(Err(format!("{e:#}")));
                return;
            }
        };
        detect(&cfg, &shared);
        for job in rx {
            match job {
                Job::Verify { user, profiles, reply } => {
                    let t = Instant::now();
                    let v = verify(&mut engine, &cfg, &shared, &profiles);
                    info!("verify {user}: {v:?} in {} ms", t.elapsed().as_millis());
                    let _ = reply.send(v);
                }
                Job::Enroll { events, cancel, reply } => {
                    let r = enroll(&mut engine, &cfg, &shared, &events, &cancel).map_err(|e| format!("{e:#}"));
                    let _ = reply.send(r);
                }
                Job::Preview { profiles, events, cancel } => {
                    if let Err(e) = preview(&mut engine, &cfg, &shared, &profiles, &events, &cancel) {
                        warn!("preview ended: {e:#}");
                    }
                }
            }
        }
    })?;
    let model_id = ready_rx.recv()?.map_err(|e| anyhow::anyhow!(e))?;
    Ok((tx, model_id))
}

fn verify(engine: &mut Engine, cfg: &Config, shared: &SharedCamera, profiles: &[Profile]) -> Verdict {
    let (feed, info) = match open_feed(cfg, shared) {
        Ok(f) => f,
        Err(e) => {
            error!("camera: {e:#}");
            return Verdict::Reject("camera");
        }
    };
    // Colour cameras never strobe: the IR checks cannot apply.
    let mut vcfg = cfg.verify.clone();
    if info.kind == CameraKind::Rgb {
        vcfg.require_strobe = false;
        vcfg.eyewear_check = false;
    }
    let mut v = Verifier::new(&vcfg, profiles, &engine.model_id);
    if !v.has_profiles() {
        return Verdict::Reject("no-profile");
    }
    let deadline = Instant::now() + Duration::from_millis(cfg.verify.timeout_ms);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let Ok(pair) = feed.next(left) else { break };
        let a = match engine.analyze(&pair) {
            Ok(a) => a,
            Err(e) => {
                error!("analyze: {e:#}");
                continue;
            }
        };
        if !v.wants_embedding(a.as_ref()) {
            continue;
        }
        let a = a.expect("wants_embedding implies a face");
        match engine.embed(&pair, &a.face) {
            Ok(e) => {
                if let Some(verdict) = v.feed(&a, &e) {
                    return verdict;
                }
            }
            Err(e) => error!("embed: {e:#}"),
        }
    }
    let verdict = v.finish();
    info!("best similarity {:.3}", v.best_similarity());
    verdict
}

fn frame_event(lit: &facelock_core::image::Gray, a: Option<&Analysis>) -> Event {
    let small = lit.resize(PREVIEW_W, PREVIEW_H);
    let k = PREVIEW_W as f64 / lit.w as f64;
    let face = a
        .map(|a| {
            let b = a.face.bbox;
            let mut v = vec![b.x as f64 * k, b.y as f64 * k, b.w as f64 * k, b.h as f64 * k];
            for p in a.face.kps {
                v.push(p[0] as f64 * k);
                v.push(p[1] as f64 * k);
            }
            v
        })
        .unwrap_or_default();
    Event::Frame { width: PREVIEW_W as u32, height: PREVIEW_H as u32, pixels: small.px, face }
}

fn enroll(
    engine: &mut Engine,
    cfg: &Config,
    shared: &SharedCamera,
    events: &mpsc::UnboundedSender<Event>,
    cancel: &AtomicBool,
) -> Result<EnrollOutcome> {
    let (feed, _) = open_feed(cfg, shared)?;
    let mut en = Enroller::new();
    let deadline = Instant::now() + ENROLL_TIMEOUT;
    while !en.is_done() {
        if cancel.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        if Instant::now() > deadline {
            anyhow::bail!("timeout");
        }
        let pair = feed.next(Duration::from_secs(3))?;
        let a = engine.analyze(&pair)?;
        let _ = events.send(frame_event(&pair.lit, a.as_ref()));
        let evs = en.feed(Instant::now(), a.as_ref(), &mut || {
            engine.embed(&pair, &a.as_ref().expect("capture implies a face").face)
        })?;
        for e in evs {
            let _ = events.send(Event::Enroll(e));
        }
    }
    en.outcome().ok_or_else(|| anyhow::anyhow!("too-few-poses"))
}

fn preview(
    engine: &mut Engine,
    cfg: &Config,
    shared: &SharedCamera,
    profiles: &[Profile],
    events: &mpsc::UnboundedSender<Event>,
    cancel: &AtomicBool,
) -> Result<()> {
    let (feed, _) = open_feed(cfg, shared)?;
    let usable: Vec<&Profile> = profiles.iter().filter(|p| p.model == engine.model_id).collect();
    let mut recent = std::collections::VecDeque::with_capacity(16);
    while !cancel.load(Ordering::Relaxed) {
        let pair = feed.next(Duration::from_secs(3))?;
        let a = engine.analyze(&pair)?;
        let _ = events.send(frame_event(&pair.lit, a.as_ref()));
        let Some(a) = a else { continue };
        let emb = engine.embed(&pair, &a.face)?;
        let sim = usable
            .iter()
            .flat_map(|p| p.samples.iter())
            .map(|s| facelock_core::embed::cosine(&emb, &s.embedding))
            .fold(-1.0f32, f32::max);
        if recent.len() == 15 {
            recent.pop_front();
        }
        recent.push_back((a.features, a.pose.yaw, a.pose.pitch));
        let obs: Vec<_> = recent.iter().copied().collect();
        let eyewear = match liveness::classify_eyewear(&obs) {
            Some(Eyewear::Glasses) => "glasses",
            Some(Eyewear::None) => "none",
            None => "",
        };
        let _ = events.send(Event::Stats {
            similarity: sim as f64,
            emitter_gain: a.features.emitter_gain.unwrap_or(0.0) as f64,
            yaw: a.pose.yaw as f64,
            pitch: a.pose.pitch as f64,
            eyewear,
        });
    }
    Ok(())
}
