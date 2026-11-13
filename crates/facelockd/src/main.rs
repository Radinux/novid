//! facelockd: IR face authentication daemon.
//!
//! `facelockd` runs on the system bus as root (systemd, D-Bus activated).
//! `facelockd --session [--config FILE]` runs as the current user on the
//! session bus for development: no polkit, own account only.

mod dbus;
mod pamconf;
mod secret;
mod worker;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use facelock_core::config::{self, Config};
use facelock_core::store::Store;
use log::info;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None) // journald adds its own
        .init();

    let mut args = std::env::args().skip(1);
    let mut session = false;
    let mut cfg_path = PathBuf::from(config::DEFAULT_PATH);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--session" => session = true,
            "--config" => cfg_path = args.next().context("--config needs a path")?.into(),
            "--version" => {
                println!("facelockd {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }

    let cfg = Arc::new(Config::load(&cfg_path)?);
    let store = Arc::new(Store::new(&cfg.storage.dir));
    let camera: worker::SharedCamera = Default::default();
    let (jobs, model_id) = worker::spawn(cfg.clone(), camera.clone()).context("loading models")?;
    info!("models loaded (recognizer {model_id})");

    let secrets = Arc::new(secret::Secrets::new(&cfg.storage.dir));
    let daemon = dbus::Daemon::new(jobs, store, secrets, cfg, model_id, camera, cfg_path, session);
    let builder = if session { zbus::connection::Builder::session() } else { zbus::connection::Builder::system() };
    let _conn = builder.name(dbus::BUS_NAME).serve_at(dbus::PATH, daemon).build().await?;
    info!("listening on the {} bus as {}", if session { "session" } else { "system" }, dbus::BUS_NAME);

    tokio::signal::ctrl_c().await?;
    info!("shutting down");
    Ok(())
}
