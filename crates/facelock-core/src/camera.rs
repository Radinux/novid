//! Camera discovery and capture.
//!
//! IR cameras (Windows Hello style) deliver 8-bit greyscale frames and strobe
//! their emitter: one frame lit by the IR LED, the next one ambient only. We
//! pair consecutive frames into (lit, dark); their difference isolates light
//! from the emitter, which is the basis of the liveness checks.
//!
//! Plain RGB webcams are supported as an explicit, weaker fallback: luma
//! feeds detection and recognition, the colour frame feeds the MiniFASNet
//! photo/screen check, and there is never a dark frame.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use v4l::buffer::Type;
use v4l::io::mmap::Stream as MmapStream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::video::capture::Parameters;
use v4l::{Device, FourCC, Format};

use crate::image::{Bgr, Gray, yuyv_to_bgr};

/// Lit frame must be at least this much brighter than its neighbour to be
/// considered an emitter frame.
const STROBE_RATIO: f32 = 1.12;
const GREY: &[u8; 4] = b"GREY";
const YUYV: &[u8; 4] = b"YUYV";

pub struct FramePair {
    pub lit: Gray,
    /// `None` when the emitter does not strobe, or on RGB cameras.
    pub dark: Option<Gray>,
    /// The same frame in colour, from colour webcams only.
    pub color: Option<Bgr>,
    pub at: Instant,
}

impl FramePair {
    /// Lit minus dark: only what the emitter illuminated.
    pub fn emitter_only(&self) -> Option<Gray> {
        self.dark.as_ref().map(|d| self.lit.sub_sat(d))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CameraConfig {
    /// "auto" or a device path (/dev/videoN, /dev/v4l/by-path/…).
    pub device: String,
    /// Fall back to a regular colour webcam when no IR camera exists.
    /// Much weaker against photos/screens: off by default.
    pub allow_rgb: bool,
    /// Preferred frame size (the closest supported one is used).
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl Default for CameraConfig {
    fn default() -> Self {
        Self { device: "auto".into(), allow_rgb: false, width: 640, height: 360, fps: 30 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CameraKind {
    Ir,
    Rgb,
}

impl CameraKind {
    pub fn code(self) -> &'static str {
        match self {
            CameraKind::Ir => "ir",
            CameraKind::Rgb => "rgb",
        }
    }
}

/// A resolved capture device.
#[derive(Clone, Debug)]
pub struct CameraInfo {
    /// /dev/videoN at detection time.
    pub node: PathBuf,
    /// /dev/v4l/by-path/… link, stable across reboots and USB enumeration.
    pub stable: Option<PathBuf>,
    pub name: String,
    pub kind: CameraKind,
    pub fourcc: [u8; 4],
    pub width: u32,
    pub height: u32,
}

impl CameraInfo {
    /// Path to open: the stable link when there is one.
    pub fn path(&self) -> &Path {
        self.stable.as_deref().unwrap_or(&self.node)
    }
}

struct Candidate {
    node: PathBuf,
    name: String,
    fourcc: [u8; 4],
    size: (u32, u32),
}

fn sizes(dev: &Device, fourcc: FourCC) -> Vec<(u32, u32)> {
    dev.enum_framesizes(fourcc)
        .unwrap_or_default()
        .into_iter()
        .flat_map(|f| f.size.to_discrete())
        .map(|d| (d.width, d.height))
        .collect()
}

/// Closest supported size to the preference, favouring ≤ 640 px wide frames
/// (more pixels only slow detection down).
fn pick_size(avail: &[(u32, u32)], want: (u32, u32)) -> Option<(u32, u32)> {
    avail.iter().copied().min_by_key(|&(w, h)| {
        let over = if w > 640 { 1_000_000 } else { 0 };
        over + (w as i64 - want.0 as i64).unsigned_abs() + (h as i64 - want.1 as i64).unsigned_abs()
    })
}

fn probe(node: &Path, cfg: &CameraConfig) -> Option<Candidate> {
    let dev = Device::with_path(node).ok()?;
    let caps = dev.query_caps().ok()?;
    let formats = dev.enum_formats().ok()?;
    let want = (cfg.width, cfg.height);
    for fourcc in [GREY, YUYV] {
        if formats.iter().any(|f| f.fourcc == FourCC::new(fourcc)) {
            let size = pick_size(&sizes(&dev, FourCC::new(fourcc)), want).unwrap_or(want);
            return Some(Candidate { node: node.to_path_buf(), name: caps.card.clone(), fourcc: *fourcc, size });
        }
    }
    None
}

fn stable_link(node: &Path) -> Option<PathBuf> {
    let target = std::fs::canonicalize(node).ok()?;
    for dir in ["/dev/v4l/by-path", "/dev/v4l/by-id"] {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        let mut links: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        links.sort();
        if let Some(l) = links.into_iter().find(|l| std::fs::canonicalize(l).is_ok_and(|t| t == target)) {
            return Some(l);
        }
    }
    None
}

impl Candidate {
    fn into_info(self) -> CameraInfo {
        let kind = if &self.fourcc == GREY { CameraKind::Ir } else { CameraKind::Rgb };
        CameraInfo {
            stable: stable_link(&self.node),
            node: self.node,
            name: self.name,
            kind,
            fourcc: self.fourcc,
            width: self.size.0,
            height: self.size.1,
        }
    }
}

/// Finds the camera to use. With `device = "auto"`: greyscale (IR) nodes
/// first, preferring one whose emitter visibly strobes; then, only if
/// `allow_rgb`, the first colour webcam.
pub fn detect(cfg: &CameraConfig) -> Result<CameraInfo> {
    if cfg.device != "auto" {
        let c = probe(Path::new(&cfg.device), cfg)
            .with_context(|| format!("{} is not a usable camera (needs GREY or YUYV frames)", cfg.device))?;
        if &c.fourcc != GREY && !cfg.allow_rgb {
            bail!("{} is a colour camera; set allow_rgb = true to use it (weaker liveness)", cfg.device);
        }
        return Ok(c.into_info());
    }

    let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev")?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("video")))
        .collect();
    nodes.sort_by_key(|p| p.file_name().and_then(|n| n.to_str()).and_then(|n| n[5..].parse::<u32>().ok()).unwrap_or(u32::MAX));
    let cands: Vec<Candidate> = nodes.iter().filter_map(|n| probe(n, cfg)).collect();

    let mut ir: Vec<Candidate> = Vec::new();
    let mut rgb: Vec<Candidate> = Vec::new();
    for c in cands {
        if &c.fourcc == GREY { ir.push(c) } else { rgb.push(c) }
    }
    if ir.len() > 1 {
        // Several greyscale nodes: keep the first one with a strobing emitter.
        let fps = cfg.fps;
        if let Some(i) = ir.iter().position(|c| strobes(c, fps)) {
            return Ok(ir.swap_remove(i).into_info());
        }
    }
    if let Some(c) = ir.into_iter().next() {
        return Ok(c.into_info());
    }
    if cfg.allow_rgb && let Some(c) = rgb.into_iter().next() {
        return Ok(c.into_info());
    }
    if cfg.allow_rgb {
        bail!("no camera found");
    }
    bail!("no IR camera found (set allow_rgb = true to use a regular webcam, with weaker liveness)")
}

/// Captures a few frames and checks for the lit/dark alternation.
fn strobes(c: &Candidate, fps: u32) -> bool {
    let info = CameraInfo {
        node: c.node.clone(),
        stable: None,
        name: c.name.clone(),
        kind: CameraKind::Ir,
        fourcc: c.fourcc,
        width: c.size.0,
        height: c.size.1,
    };
    let Ok(mut cam) = Camera::open(&info, fps) else { return false };
    let mut lit_pairs = 0;
    for _ in 0..4 {
        match cam.next_pair(Duration::from_millis(800)) {
            Ok(p) if p.dark.is_some() => lit_pairs += 1,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    lit_pairs >= 3
}

pub struct Camera {
    // Keep the device alive as long as the stream.
    _dev: Device,
    stream: MmapStream<'static>,
    kind: CameraKind,
    yuyv: bool,
    w: usize,
    h: usize,
    stride: usize,
    prev: Option<(Gray, f32)>,
    non_strobe_streak: u32,
}

impl Camera {
    pub fn open(info: &CameraInfo, fps: u32) -> Result<Self> {
        let path = info.path();
        let dev = Device::with_path(path).with_context(|| format!("opening {}", path.display()))?;
        let want = Format::new(info.width, info.height, FourCC::new(&info.fourcc));
        let got = dev.set_format(&want).context("setting capture format")?;
        if got.fourcc != FourCC::new(&info.fourcc) {
            bail!("{} refused {} frames (got {})", path.display(), FourCC::new(&info.fourcc), got.fourcc);
        }
        // Not fatal if the driver refuses: we just get its default rate.
        let _ = dev.set_params(&Parameters::with_fps(fps));
        let stream = MmapStream::with_buffers(&dev, Type::VideoCapture, 4).context("starting capture stream")?;
        let yuyv = &info.fourcc == YUYV;
        let min_stride = got.width * if yuyv { 2 } else { 1 };
        Ok(Self {
            _dev: dev,
            stream,
            kind: info.kind,
            yuyv,
            w: got.width as usize,
            h: got.height as usize,
            stride: got.stride.max(min_stride) as usize,
            prev: None,
            non_strobe_streak: 0,
        })
    }

    pub fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    fn grab(&mut self) -> Result<(Gray, Option<Bgr>)> {
        let (buf, _meta) = self.stream.next().context("reading frame")?;
        if buf.len() < self.stride * (self.h - 1) + self.w * if self.yuyv { 2 } else { 1 } {
            bail!("short frame: {} bytes", buf.len());
        }
        let mut px = Vec::with_capacity(self.w * self.h);
        let mut bgr = Vec::with_capacity(if self.yuyv { self.w * self.h * 3 } else { 0 });
        for y in 0..self.h {
            let row = &buf[y * self.stride..];
            if self.yuyv {
                // Y0 U Y1 V: luma is every other byte.
                px.extend(row[..self.w * 2].iter().step_by(2));
                yuyv_to_bgr(&row[..self.w * 2], &mut bgr);
            } else {
                px.extend_from_slice(&row[..self.w]);
            }
        }
        let color = self.yuyv.then(|| Bgr::from_raw(self.w, self.h, bgr));
        Ok((Gray::from_raw(self.w, self.h, px), color))
    }

    /// Returns the next (lit, dark) pair. Blocks for at most `timeout`.
    pub fn next_pair(&mut self, timeout: Duration) -> Result<FramePair> {
        if self.kind == CameraKind::Rgb {
            let (lit, color) = self.grab()?;
            return Ok(FramePair { lit, dark: None, color, at: Instant::now() });
        }
        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() > deadline {
                bail!("timed out waiting for IR frames");
            }
            let (cur, _) = self.grab()?;
            let m = cur.mean();
            let Some((prev, pm)) = self.prev.take() else {
                self.prev = Some((cur, m));
                continue;
            };
            if m > pm * STROBE_RATIO {
                self.non_strobe_streak = 0;
                return Ok(FramePair { lit: cur, dark: Some(prev), color: None, at: Instant::now() });
            }
            if pm > m * STROBE_RATIO {
                self.non_strobe_streak = 0;
                return Ok(FramePair { lit: prev, dark: Some(cur), color: None, at: Instant::now() });
            }
            self.non_strobe_streak += 1;
            self.prev = Some((cur, m));
            if self.non_strobe_streak >= 6 {
                // Emitter isn't strobing (or is off). Hand out single frames so
                // recognition still works; liveness will mark it as weak.
                let (f, _) = self.prev.take().unwrap();
                return Ok(FramePair { lit: f, dark: None, color: None, at: Instant::now() });
            }
        }
    }
}
