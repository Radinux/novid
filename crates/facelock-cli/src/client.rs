//! Commands that go through facelockd over D-Bus.

use std::io::Write;

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use zbus::message::Type;
use zbus::{Connection, MatchRule, MessageStream, proxy};

const IFACE: &str = "org.facelock.Daemon1";

#[proxy(interface = "org.facelock.Daemon1", default_service = "org.facelock.Daemon1", default_path = "/org/facelock/Daemon1")]
pub trait Daemon1 {
    fn verify(&self, user: &str) -> zbus::Result<(bool, String)>;
    #[allow(clippy::type_complexity)]
    fn list_profiles(&self, user: &str) -> zbus::Result<Vec<(String, String, u64, String, u32, bool)>>;
    fn start_enroll(&self, user: &str, label: &str) -> zbus::Result<()>;
    fn cancel(&self) -> zbus::Result<()>;
    fn delete_profile(&self, user: &str, id: &str) -> zbus::Result<()>;
    fn rename_profile(&self, user: &str, id: &str, label: &str) -> zbus::Result<()>;
}

pub async fn connect(session: bool) -> Result<(Connection, Daemon1Proxy<'static>)> {
    let conn = if session { Connection::session().await? } else { Connection::system().await? };
    let proxy = Daemon1Proxy::new(&conn).await.context("connecting to facelockd")?;
    Ok((conn, proxy))
}

pub fn reason_fr(code: &str) -> &str {
    match code {
        "ok" => "reconnu",
        "no-profile" => "aucun visage enregistré",
        "no-face" => "aucun visage détecté",
        "no-match" => "visage non reconnu",
        "no-ir-strobe" => "l'émetteur IR ne clignote pas",
        "low-ir" => "trop peu de lumière IR réfléchie (écran ? trop loin ?)",
        "eyewear-mismatch" => "reflets incohérents avec le profil (lunettes ?)",
        "eyewear-undecided" => "regarde l'écran bien en face",
        "cooldown" => "trop d'échecs, réessaie dans un moment",
        "camera" => "caméra indisponible",
        other => other,
    }
}

pub async fn verify(p: &Daemon1Proxy<'_>, user: &str) -> Result<()> {
    let t = std::time::Instant::now();
    let (ok, reason) = p.verify(user).await?;
    let ms = t.elapsed().as_millis();
    if ok {
        println!("✅ {user} : {} ({ms} ms)", reason_fr(&reason));
        Ok(())
    } else {
        println!("❌ {user} : {} ({ms} ms)", reason_fr(&reason));
        std::process::exit(1);
    }
}

pub async fn list(p: &Daemon1Proxy<'_>, user: &str) -> Result<()> {
    let profiles = p.list_profiles(user).await?;
    if profiles.is_empty() {
        println!("Aucun visage enregistré pour {user}.");
    }
    for (id, label, created, eyewear, n, usable) in profiles {
        let glasses = match eyewear.as_str() {
            "glasses" => "👓 lunettes",
            "none" => "👁  sans lunettes",
            _ => "?  indéterminé",
        };
        let warn = if usable { "" } else { "  ⚠ autre modèle, à refaire" };
        println!("{id}  {label:<20}  {glasses:<16} {n} poses  créé {}{warn}", fmt_time(created));
    }
    Ok(())
}

fn fmt_time(t: u64) -> String {
    std::process::Command::new("date")
        .args(["-d", &format!("@{t}"), "+%d/%m/%Y %H:%M"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| t.to_string())
}

fn instruction_fr(code: &str) -> &str {
    match code {
        "look-front" => "Regarde l'écran, bien en face",
        "recenter" => "Reviens au centre, face à l'écran",
        "turn-left" => "Tourne doucement la tête vers ta GAUCHE ←",
        "turn-left-more" => "Encore un peu plus à GAUCHE ←←",
        "turn-right" => "Maintenant vers ta DROITE →",
        "turn-right-more" => "Encore un peu plus à DROITE →→",
        "look-up" => "Reviens au centre et LÈVE un peu le menton ↑",
        "look-down" => "Maintenant BAISSE un peu la tête ↓",
        other => other,
    }
}

fn hint_fr(code: &str, axis: &str) -> &'static str {
    match code {
        "no-face" => "visage perdu — reviens face à la caméra",
        "more" => "un peu plus…",
        "hold" => "tiens la position…",
        "overshoot" => "un peu trop, reviens légèrement",
        "wrong-way" => "pas par là, de l'autre côté",
        "center" => "reviens au centre",
        "straighten" if axis == "yaw" => "garde la tête droite (ni haut ni bas)",
        "straighten" => "recentre la tête (ni gauche ni droite)",
        _ => "",
    }
}

/// Gauge from -40° to +40°; for yaw the user's left is drawn on the left.
fn gauge(axis: &str, value: f64, target: f64) -> String {
    const W: usize = 31;
    let col = |v: f64| {
        let v = if axis == "yaw" { -v } else { v };
        (((v + 40.0) / 80.0) * (W - 1) as f64).round().clamp(0.0, (W - 1) as f64) as usize
    };
    let mut g = vec!['─'; W];
    g[W / 2] = '┼';
    g[col(target)] = '◎';
    g[col(value)] = '●';
    let (l, r) = if axis == "yaw" { ("gauche", "droite") } else { ("  bas ", " haut ") };
    format!("{l} ├{}┤ {r}", g.into_iter().collect::<String>())
}

pub async fn enroll(conn: &Connection, p: &Daemon1Proxy<'_>, user: &str, label: &str) -> Result<()> {
    // Subscribe before starting so no signal is missed.
    let rule = MatchRule::builder().msg_type(Type::Signal).interface(IFACE).build()?;
    let mut stream = MessageStream::for_match_rule(rule, conn, None).await?;
    p.start_enroll(user, label).await.context("starting enrollment")?;

    println!("\nPlace-toi face à l'écran, à distance normale. Mesure de référence…");
    let (mut axis, mut target) = (String::from("yaw"), 0.0f64);
    let mut out = std::io::stdout();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                let _ = p.cancel().await;
                println!("\nAnnulé.");
                bail!("cancelled");
            }
            msg = stream.next() => {
                let Some(msg) = msg else { bail!("connexion perdue") };
                let msg = msg?;
                let hdr = msg.header();
                let Some(member) = hdr.member() else { continue };
                let body = msg.body();
                match member.as_str() {
                    "EnrollCalibrating" => {
                        let progress: f64 = body.deserialize()?;
                        if progress >= 1.0 {
                            println!("\x1b[2K\rRéférence OK. Suis les consignes, lentement, avec une petite pause à chaque position.\n");
                        }
                    }
                    "EnrollStep" => {
                        let (i, n, code, ax, tg): (u32, u32, String, String, f64) = body.deserialize()?;
                        (axis, target) = (ax, tg);
                        println!("\x1b[1mÉtape {}/{n} — {}\x1b[0m", i + 1, instruction_fr(&code));
                    }
                    "EnrollProgress" => {
                        let (v, _off, hint): (f64, f64, String) = body.deserialize()?;
                        let g = if hint == "no-face" { String::new() } else { gauge(&axis, v, target) };
                        print!("\x1b[2K\r  {g}   {}", hint_fr(&hint, &axis));
                        out.flush().ok();
                    }
                    "EnrollCaptured" => println!("\x1b[2K\r  \x1b[32m✓ capturé\x1b[0m"),
                    "EnrollSkipped" => println!("\x1b[2K\r  ⏭  position non atteinte, on passe à la suite"),
                    "EnrollFinished" => {
                        let (ok, id, err): (bool, String, String) = body.deserialize()?;
                        if ok {
                            println!("\n✅ Visage enregistré (profil {id}).");
                            return Ok(());
                        }
                        let why = match err.as_str() {
                            "too-few-poses" => "pas assez de positions capturées",
                            "timeout" => "temps écoulé",
                            "cancelled" => "annulé",
                            e => e,
                        };
                        bail!("échec de l'enrôlement : {why}");
                    }
                    _ => {}
                }
            }
        }
    }
}
