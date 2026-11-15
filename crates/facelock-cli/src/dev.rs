//! Developer tools that open the camera directly (daemon must not be using it).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use facelock_core::align;
use facelock_core::camera::{self, Camera, FramePair};
use facelock_core::config::Config;
use facelock_core::embed::cosine;
use facelock_core::engine::{Analysis, Engine};
use facelock_core::image::Gray;
use facelock_core::liveness::{self, Eyewear, LivenessFeatures};
use facelock_core::store::{Profile, Store};

type Obs = (LivenessFeatures, f32, f32);

fn profiles(cfg: &Config, user: &str, model: &str) -> Vec<Profile> {
    Store::new(&cfg.storage.dir)
        .load(user)
        .map(|s| s.profiles.into_iter().filter(|p| p.model == model).collect())
        .unwrap_or_default()
}

fn print_line(a: &Analysis, sim: Option<f32>, ms: u128) {
    let f = &a.features;
    let opt = |v: Option<f32>, p: usize| v.map_or("-".into(), |x| format!("{x:.p$}"));
    println!(
        "sim {:>5}  {ms:>3}ms  face {:.2} iod {:4.1} yaw {:+5.1} pitch {:+5.1} roll {:+5.1} | gain {:>5} glint {:>4} area {:>4} off {:>4} | lit {:5.1} sharp {:4.1} | réel {:>4}",
        opt(sim, 3),
        a.face.score,
        a.face.iod(),
        a.pose.yaw,
        a.pose.pitch,
        a.pose.roll,
        opt(f.emitter_gain, 1),
        opt(f.glint, 2),
        opt(f.glint_area, 1),
        opt(f.glint_offset, 2),
        f.lit_level,
        f.sharpness,
        opt(a.real_score, 2)
    );
}

fn process(engine: &mut Engine, profs: &[Profile], pair: &FramePair, t: Instant) -> Result<Option<Obs>> {
    let Some(a) = engine.analyze(pair)? else {
        println!("—");
        return Ok(None);
    };
    let sim = if profs.is_empty() {
        None
    } else {
        let e = engine.embed(pair, &a.face)?;
        Some(profs.iter().flat_map(|p| &p.samples).map(|s| cosine(&e, &s.embedding)).fold(-1.0, f32::max))
    };
    print_line(&a, sim, t.elapsed().as_millis());
    Ok(Some((a.features, a.pose.yaw, a.pose.pitch)))
}

fn print_eyewear(obs: &[Obs]) {
    match liveness::classify_eyewear(obs) {
        Some(Eyewear::Glasses) => println!("reflets : lunettes 👓"),
        Some(Eyewear::None) => println!("reflets : yeux nus 👁"),
        None => println!("reflets : pas assez d'images de face pour conclure"),
    }
}

pub fn test(cfg: &Config, user: &str, seconds: u64, record: Option<&Path>) -> Result<()> {
    let mut engine = Engine::load(&cfg.models)?;
    let profs = profiles(cfg, user, &engine.model_id);
    if profs.is_empty() {
        println!("(aucun profil pour {user} dans {}, indices seulement)", cfg.storage.dir.display());
    }
    if let Some(dir) = record {
        std::fs::create_dir_all(dir)?;
    }
    let info = camera::detect(&cfg.camera)?;
    println!("caméra : {} ({}) {}", info.name, info.kind.code(), info.path().display());
    let mut cam = Camera::open(&info, cfg.camera.fps)?;
    let (end, t0) = (Instant::now() + Duration::from_secs(seconds), Instant::now());
    let (mut n, mut obs) = (0u32, Vec::new());
    while Instant::now() < end {
        let t = Instant::now();
        let pair = cam.next_pair(Duration::from_secs(3))?;
        if let Some(dir) = record {
            save_gray(&pair.lit, &dir.join(format!("{n:04}_lit.png")))?;
            if let Some(d) = &pair.dark {
                save_gray(d, &dir.join(format!("{n:04}_dark.png")))?;
            }
        }
        n += 1;
        obs.extend(process(&mut engine, &profs, &pair, t)?);
    }
    print_eyewear(&obs);
    println!("{:.1} paires/s", n as f32 / t0.elapsed().as_secs_f32());
    if let Some(dir) = record {
        println!("{n} paires enregistrées dans {}", dir.display());
    }
    Ok(())
}

pub fn replay(cfg: &Config, user: &str, dir: &Path) -> Result<()> {
    let mut engine = Engine::load(&cfg.models)?;
    let profs = profiles(cfg, user, &engine.model_id);
    let mut lits: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().ends_with("_lit.png"))
        .collect();
    lits.sort();
    let mut obs = Vec::new();
    for lit_path in lits {
        let dark_path = PathBuf::from(lit_path.to_string_lossy().replace("_lit.png", "_dark.png"));
        let dark = if dark_path.exists() { Some(load_gray(&dark_path)?) } else { None };
        let pair = FramePair { lit: load_gray(&lit_path)?, dark, color: None, at: Instant::now() };
        obs.extend(process(&mut engine, &profs, &pair, Instant::now())?);
    }
    print_eyewear(&obs);
    Ok(())
}

pub fn snapshot(cfg: &Config, out: &Path) -> Result<()> {
    let mut engine = Engine::load(&cfg.models)?;
    let info = camera::detect(&cfg.camera)?;
    println!("caméra : {} ({}) {}", info.name, info.kind.code(), info.path().display());
    let mut cam = Camera::open(&info, cfg.camera.fps)?;
    for _ in 0..8 {
        cam.next_pair(Duration::from_secs(3))?; // let auto-exposure settle
    }
    let pair = cam.next_pair(Duration::from_secs(3))?;
    let a = engine.analyze(&pair)?;
    let blank = || Gray::new(pair.lit.w, pair.lit.h);
    let dark = pair.dark.clone().unwrap_or_else(blank);
    let mut diff = pair.emitter_only().unwrap_or_else(blank);
    let mx = (*diff.px.iter().max().unwrap_or(&1) as f32).max(1.0);
    diff.px.iter_mut().for_each(|v| *v = (*v as f32 * 255.0 / mx) as u8);

    let mut panels = vec![pair.lit.clone(), dark, diff];
    if let Some(a) = &a {
        panels.iter_mut().for_each(|p| draw_face(p, a));
        panels.push(align::align(&pair.lit, &a.face, 112).resize(pair.lit.h, pair.lit.h));
        print_line(a, None, 0);
    } else {
        println!("aucun visage détecté");
    }
    let (w, h) = (panels.iter().map(|p| p.w).sum::<usize>(), pair.lit.h);
    let mut canvas = image::GrayImage::new(w as u32, h as u32);
    let mut ox = 0;
    for p in &panels {
        for y in 0..p.h.min(h) {
            for x in 0..p.w {
                canvas.put_pixel((ox + x) as u32, y as u32, image::Luma([p.at(x, y)]));
            }
        }
        ox += p.w;
    }
    canvas.save(out)?;
    println!("enregistré : {} (éclairé | sombre | émetteur seul | visage aligné)", out.display());
    Ok(())
}

fn draw_face(img: &mut Gray, a: &Analysis) {
    let (x0, y0, x1, y1) = a.face.bbox.clip(img.w, img.h);
    let w = img.w;
    let mut set = |x: usize, y: usize| img.px[y * w + x] = 255;
    for x in x0..x1 {
        set(x, y0);
        set(x, y1.saturating_sub(1));
    }
    for y in y0..y1 {
        set(x0, y);
        set(x1.saturating_sub(1), y);
    }
    for k in a.face.kps {
        for dy in -2i32..=2 {
            for dx in -2i32..=2 {
                let (x, y) = (k[0] as i32 + dx, k[1] as i32 + dy);
                if x >= 0 && y >= 0 && (x as usize) < img.w && (y as usize) < img.h {
                    img.px[y as usize * img.w + x as usize] = 255;
                }
            }
        }
    }
}

fn save_gray(g: &Gray, path: &Path) -> Result<()> {
    image::GrayImage::from_raw(g.w as u32, g.h as u32, g.px.clone()).context("bad frame")?.save(path)?;
    Ok(())
}

fn load_gray(path: &Path) -> Result<Gray> {
    let img = image::open(path)?.into_luma8();
    Ok(Gray::from_raw(img.width() as usize, img.height() as usize, img.into_raw()))
}
