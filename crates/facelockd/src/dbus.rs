//! `org.facelock.Daemon1` on the system bus.
//!
//! Authorisation model:
//! * `Verify(user)`: root for anyone (PAM in sudo/gdm/polkit), otherwise only
//!   for the caller's own account (screen lockers run PAM as the user).
//! * Managing faces / preview: polkit `org.facelock.enroll` for one's own
//!   account, `org.facelock.manage` for someone else's.
//! * Camera frames are sent as *unicast* signals to the requesting client only.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use facelock_core::config::Config;
use facelock_core::enroll::EnrollEvent;
use facelock_core::liveness::Eyewear;
use facelock_core::store::{self, Profile, Store};
use facelock_core::verify::Verdict;
use futures_util::StreamExt;
use log::{info, warn};
use tokio::sync::{mpsc, oneshot};
use zbus::fdo;
use zbus::message::Header;
use zbus::names::{BusName, UniqueName};
use zbus::object_server::SignalEmitter;
use zbus::Variant as Value;
use zbus::{Connection, interface, proxy};

use crate::pamconf::{self, PamFiles};
use crate::secret::{self, Secrets};
use crate::worker::{Event, Job, SharedCamera};

pub const BUS_NAME: &str = "org.facelock.Daemon1";
pub const PATH: &str = "/org/facelock/Daemon1";
const ACTION_ENROLL: &str = "org.facelock.enroll";
const ACTION_MANAGE: &str = "org.facelock.manage";
const ACTION_CONFIGURE: &str = "org.facelock.configure";

#[proxy(
    interface = "org.freedesktop.PolicyKit1.Authority",
    default_service = "org.freedesktop.PolicyKit1",
    default_path = "/org/freedesktop/PolicyKit1/Authority"
)]
trait Authority {
    #[allow(clippy::type_complexity)]
    fn check_authorization(
        &self,
        subject: &(&str, HashMap<&str, Value<'_>>),
        action_id: &str,
        details: HashMap<&str, &str>,
        flags: u32,
        cancellation_id: &str,
    ) -> zbus::Result<(bool, bool, HashMap<String, String>)>;
}

struct Interactive {
    owner: String,
    cancel: Arc<AtomicBool>,
}

pub struct Daemon {
    pub jobs: std::sync::mpsc::Sender<Job>,
    pub store: Arc<Store>,
    pub secrets: Arc<Secrets>,
    pub cfg: Arc<Config>,
    pub model_id: String,
    pub camera: SharedCamera,
    pub pam: PamFiles,
    pub config_path: std::path::PathBuf,
    /// Session-bus developer mode: no polkit, only the current user.
    pub dev_mode: bool,
    interactive: Arc<Mutex<Option<Interactive>>>,
    failures: Mutex<HashMap<String, (u32, Instant)>>,
}

fn denied(msg: &str) -> fdo::Error {
    fdo::Error::AccessDenied(msg.into())
}

fn failed(e: impl std::fmt::Display) -> fdo::Error {
    fdo::Error::Failed(e.to_string())
}

fn uid_of(user: &str) -> fdo::Result<u32> {
    if !store::valid_username(user) {
        return Err(fdo::Error::InvalidArgs("invalid user name".into()));
    }
    match nix::unistd::User::from_name(user) {
        Ok(Some(u)) => Ok(u.uid.as_raw()),
        Ok(None) => Err(fdo::Error::InvalidArgs(format!("no such user: {user}"))),
        Err(e) => Err(failed(e)),
    }
}

fn sender(hdr: &Header<'_>) -> fdo::Result<UniqueName<'static>> {
    hdr.sender().map(|s| s.to_owned()).ok_or_else(|| denied("no sender"))
}

async fn caller_uid(conn: &Connection, hdr: &Header<'_>) -> fdo::Result<u32> {
    let s = sender(hdr)?;
    fdo::DBusProxy::new(conn).await?.get_connection_unix_user(BusName::Unique(s)).await
}

fn eyewear_code(e: Option<Eyewear>) -> &'static str {
    match e {
        Some(Eyewear::Glasses) => "glasses",
        Some(Eyewear::None) => "none",
        None => "",
    }
}

impl Daemon {
    pub fn new(
        jobs: std::sync::mpsc::Sender<Job>,
        store: Arc<Store>,
        secrets: Arc<Secrets>,
        cfg: Arc<Config>,
        model_id: String,
        camera: SharedCamera,
        config_path: std::path::PathBuf,
        dev_mode: bool,
    ) -> Self {
        let pam = PamFiles::new(&cfg.storage.dir);
        Self {
            jobs,
            store,
            secrets,
            cfg,
            model_id,
            camera,
            pam,
            config_path,
            dev_mode,
            interactive: Arc::new(Mutex::new(None)),
            failures: Mutex::new(HashMap::new()),
        }
    }

    /// Checks that the caller may manage `user`'s faces.
    async fn authorize_manage(&self, conn: &Connection, hdr: &Header<'_>, user: &str) -> fdo::Result<()> {
        let uid = caller_uid(conn, hdr).await?;
        let target = uid_of(user)?;
        if self.dev_mode {
            return if uid == target { Ok(()) } else { Err(denied("dev mode: own account only")) };
        }
        if uid == 0 {
            return Ok(());
        }
        let action = if uid == target { ACTION_ENROLL } else { ACTION_MANAGE };
        let subject = ("system-bus-name", HashMap::from([("name", Value::from(sender(hdr)?.as_str().to_owned()))]));
        let auth = AuthorityProxy::new(conn).await?;
        // flags = 1: AllowUserInteraction (polkit agent may prompt).
        let (ok, _, _) = auth.check_authorization(&subject, action, HashMap::new(), 1, "").await.map_err(failed)?;
        if ok { Ok(()) } else { Err(denied("not authorized")) }
    }

    /// Shared by Verify and VerifyAndRelease: cooldown, camera job, counters.
    async fn run_verify(&self, user: &str) -> fdo::Result<(bool, String)> {
        {
            let f = self.failures.lock().unwrap();
            if let Some((n, last)) = f.get(user)
                && *n >= self.cfg.verify.max_failures
                && last.elapsed().as_secs() < self.cfg.verify.cooldown_s
            {
                return Ok((false, "cooldown".into()));
            }
        }
        let profiles = self.load(user)?.profiles;
        if profiles.is_empty() {
            return Ok((false, "no-profile".into()));
        }
        // Authentication takes priority over previews/enrollment.
        self.cancel_interactive();
        let (tx, rx) = oneshot::channel();
        self.jobs.send(Job::Verify { user: user.to_string(), profiles, reply: tx }).map_err(failed)?;
        let verdict = rx.await.map_err(failed)?;
        let mut f = self.failures.lock().unwrap();
        Ok(match verdict {
            Verdict::Match { profile_id, similarity } => {
                info!("auth OK for {user} (profile {profile_id}, sim {similarity:.3})");
                f.remove(user);
                (true, "ok".into())
            }
            Verdict::Reject(reason) => {
                warn!("auth FAILED for {user}: {reason}");
                let e = f.entry(user.to_string()).or_insert((0, Instant::now()));
                e.0 += 1;
                e.1 = Instant::now();
                (false, reason.into())
            }
        })
    }

    /// System-wide changes: root, or polkit `org.facelock.configure` (admin).
    async fn authorize_configure(&self, conn: &Connection, hdr: &Header<'_>) -> fdo::Result<()> {
        if self.dev_mode {
            return Err(denied("system settings are not available in dev mode"));
        }
        if caller_uid(conn, hdr).await? == 0 {
            return Ok(());
        }
        let subject = ("system-bus-name", HashMap::from([("name", Value::from(sender(hdr)?.as_str().to_owned()))]));
        let auth = AuthorityProxy::new(conn).await?;
        let (ok, _, _) =
            auth.check_authorization(&subject, ACTION_CONFIGURE, HashMap::new(), 1, "").await.map_err(failed)?;
        if ok { Ok(()) } else { Err(denied("not authorized")) }
    }

    fn load(&self, user: &str) -> fdo::Result<store::UserStore> {
        self.store.load(user).map_err(failed)
    }

    fn cancel_interactive(&self) {
        if let Some(i) = self.interactive.lock().unwrap().take() {
            i.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Registers an interactive job for `owner`; refuses if another client has one.
    fn claim_interactive(&self, owner: &str) -> fdo::Result<Arc<AtomicBool>> {
        let mut slot = self.interactive.lock().unwrap();
        if let Some(i) = slot.as_ref()
            && !i.cancel.load(Ordering::Relaxed)
        {
            if i.owner != owner {
                return Err(fdo::Error::LimitsExceeded("camera busy".into()));
            }
            i.cancel.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        *slot = Some(Interactive { owner: owner.to_string(), cancel: cancel.clone() });
        Ok(cancel)
    }

    /// Forwards worker events as unicast signals; cancels if the client leaves.
    fn pump(
        &self,
        conn: Connection,
        owner: UniqueName<'static>,
        mut rx: mpsc::UnboundedReceiver<Event>,
        cancel: Arc<AtomicBool>,
    ) {
        tokio::spawn(async move {
            let Ok(em) = SignalEmitter::new(&conn, PATH) else { return };
            let em = em.set_destination(BusName::Unique(owner.clone()));
            let mut gone = match fdo::DBusProxy::new(&conn).await {
                Ok(p) => p.receive_name_owner_changed_with_args(&[(0, owner.as_str())]).await.ok(),
                Err(_) => None,
            };
            loop {
                tokio::select! {
                    ev = rx.recv() => {
                        let Some(ev) = ev else { break };
                        if let Err(e) = emit(&em, ev).await {
                            warn!("signal to {owner}: {e}");
                        }
                    }
                    Some(_) = async { match gone.as_mut() { Some(s) => s.next().await, None => std::future::pending().await } } => {
                        info!("{owner} left the bus, cancelling its job");
                        cancel.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }
        });
    }
}

async fn emit(em: &SignalEmitter<'_>, ev: Event) -> zbus::Result<()> {
    match ev {
        Event::Frame { width, height, pixels, face } => Daemon::preview_frame(em, width, height, &pixels, &face).await,
        Event::Stats { similarity, emitter_gain, real_score, yaw, pitch, eyewear } => {
            Daemon::preview_stats(em, similarity, emitter_gain, real_score, yaw, pitch, eyewear).await
        }
        Event::Enroll(e) => match e {
            EnrollEvent::Calibrating { progress } => Daemon::enroll_calibrating(em, progress as f64).await,
            EnrollEvent::Step { index, total, code, axis, target } => {
                Daemon::enroll_step(em, index as u32, total as u32, code, axis.code(), target as f64).await
            }
            EnrollEvent::Progress { value, off_axis, hint } => {
                Daemon::enroll_progress(em, value as f64, off_axis as f64, hint.code()).await
            }
            EnrollEvent::Captured { index } => Daemon::enroll_captured(em, index as u32).await,
            EnrollEvent::Skipped { index } => Daemon::enroll_skipped(em, index as u32).await,
        },
    }
}

#[interface(name = "org.facelock.Daemon1")]
impl Daemon {
    /// Authenticates `user` with the camera. Returns (success, reason code).
    async fn verify(
        &self,
        user: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<(bool, String)> {
        let uid = caller_uid(conn, &hdr).await?;
        let target = uid_of(&user)?;
        if uid != 0 && uid != target {
            return Err(denied("may only verify your own account"));
        }
        self.run_verify(&user).await
    }

    /// Like Verify, and on success also returns the user's sealed login
    /// keyring password (empty if none). Root only: meant for the PAM module.
    async fn verify_and_release(
        &self,
        user: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<(bool, String, String)> {
        if caller_uid(conn, &hdr).await? != 0 {
            return Err(denied("root only"));
        }
        uid_of(&user)?;
        let (ok, reason) = self.run_verify(&user).await?;
        if !ok {
            return Ok((false, reason, String::new()));
        }
        let rgb = self.camera.lock().unwrap().as_ref().is_some_and(|c| c.kind == facelock_core::camera::CameraKind::Rgb);
        if rgb {
            // Without IR liveness a photo could pass: never hand out the password.
            warn!("not releasing keyring secret for {user}: colour camera");
            return Ok((true, reason, String::new()));
        }
        let secrets = self.secrets.clone();
        let u = user.clone();
        let secret = tokio::task::spawn_blocking(move || secrets.unseal(&u)).await.map_err(failed)?;
        match secret {
            Ok(Some(s)) => {
                info!("released keyring secret for {user}");
                Ok((true, reason, s.to_string()))
            }
            Ok(None) => Ok((true, reason, String::new())),
            Err(e) => {
                // Face is fine; only the keyring stays locked (e.g. PCR change).
                warn!("could not unseal keyring secret for {user}: {e:#}");
                Ok((true, reason, String::new()))
            }
        }
    }

    /// Seals the login password so face logins can unlock the keyring.
    /// The password is checked against /etc/shadow first.
    async fn set_keyring_password(
        &self,
        user: String,
        password: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        let password = zeroize::Zeroizing::new(password);
        let uid = caller_uid(conn, &hdr).await?;
        if uid != 0 && uid != uid_of(&user)? {
            return Err(denied("own account only"));
        }
        self.authorize_manage(conn, &hdr, &user).await?;
        let (secrets, u) = (self.secrets.clone(), user.clone());
        let res = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            if !secret::verify_password(&u, &password)? {
                return Ok(false);
            }
            secrets.seal(&u, &password)?;
            Ok(true)
        })
        .await
        .map_err(failed)?;
        match res {
            Ok(true) => {
                info!("keyring secret sealed for {user}");
                Ok(())
            }
            Ok(false) => {
                warn!("wrong password given to SetKeyringPassword for {user}");
                Err(fdo::Error::InvalidArgs("wrong-password".into()))
            }
            Err(e) => Err(failed(format!("{e:#}"))),
        }
    }

    async fn clear_keyring_password(
        &self,
        user: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        self.authorize_manage(conn, &hdr, &user).await?;
        self.secrets.clear(&user).map_err(failed)?;
        info!("keyring secret removed for {user}");
        Ok(())
    }

    async fn has_keyring_password(
        &self,
        user: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<bool> {
        let uid = caller_uid(conn, &hdr).await?;
        if uid != 0 && uid != uid_of(&user)? {
            return Err(denied("own account only"));
        }
        Ok(self.secrets.exists(&user))
    }

    /// (id, label, created unix time, eyewear, sample count, usable with current model)
    async fn list_profiles(
        &self,
        user: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<Vec<(String, String, u64, String, u32, bool)>> {
        let uid = caller_uid(conn, &hdr).await?;
        if uid != 0 && uid != uid_of(&user)? {
            self.authorize_manage(conn, &hdr, &user).await?;
        }
        Ok(self
            .load(&user)?
            .profiles
            .iter()
            .map(|p| {
                (
                    p.id.clone(),
                    p.label.clone(),
                    p.created,
                    eyewear_code(p.eyewear).into(),
                    p.samples.len() as u32,
                    p.model == self.model_id,
                )
            })
            .collect())
    }

    /// Starts guided enrollment. Progress arrives as Enroll* and PreviewFrame
    /// signals; the end as EnrollFinished.
    async fn start_enroll(
        &self,
        user: String,
        label: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        self.authorize_manage(conn, &hdr, &user).await?;
        let label: String = label.trim().chars().take(64).collect();
        let owner = sender(&hdr)?;
        let cancel = self.claim_interactive(owner.as_str())?;
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let (tx, rx) = oneshot::channel();
        self.jobs.send(Job::Enroll { events: ev_tx, cancel: cancel.clone(), reply: tx }).map_err(failed)?;
        self.pump(conn.clone(), owner.clone(), ev_rx, cancel);

        let (store, model, conn, interactive) =
            (self.store.clone(), self.model_id.clone(), conn.clone(), self.interactive.clone());
        tokio::spawn(async move {
            let result = match rx.await {
                Ok(Ok(outcome)) => {
                    let save = || -> anyhow::Result<String> {
                        let mut us = store.load(&user)?;
                        let id = store::new_id();
                        let n = us.profiles.len() + 1;
                        us.profiles.push(Profile {
                            id: id.clone(),
                            label: if label.is_empty() { format!("Visage {n}") } else { label },
                            created: store::now_unix(),
                            model,
                            eyewear: outcome.eyewear,
                            samples: outcome.samples,
                        });
                        store.save(&user, &us)?;
                        Ok(id)
                    };
                    save().map_err(|e| format!("{e:#}"))
                }
                Ok(Err(e)) => Err(e),
                Err(_) => Err("worker gone".into()),
            };
            interactive.lock().unwrap().take();
            let em = SignalEmitter::new(&conn, PATH).map(|e| e.set_destination(BusName::Unique(owner)));
            if let Ok(em) = em {
                let _ = match &result {
                    Ok(id) => {
                        info!("enrolled profile {id} for {user}");
                        Daemon::enroll_finished(&em, true, id, "").await
                    }
                    Err(e) => {
                        warn!("enrollment for {user} failed: {e}");
                        Daemon::enroll_finished(&em, false, "", e).await
                    }
                };
            }
        });
        Ok(())
    }

    /// Streams IR frames and live match/liveness stats (PreviewFrame, PreviewStats).
    async fn start_preview(
        &self,
        user: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        self.authorize_manage(conn, &hdr, &user).await?;
        let owner = sender(&hdr)?;
        let cancel = self.claim_interactive(owner.as_str())?;
        let profiles = self.load(&user)?.profiles;
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        self.jobs.send(Job::Preview { profiles, events: ev_tx, cancel: cancel.clone() }).map_err(failed)?;
        self.pump(conn.clone(), owner, ev_rx, cancel);
        Ok(())
    }

    /// Stops the caller's enrollment or preview.
    async fn cancel(&self, #[zbus(header)] hdr: Header<'_>) -> fdo::Result<()> {
        let owner = sender(&hdr)?;
        let mut slot = self.interactive.lock().unwrap();
        if slot.as_ref().is_some_and(|i| i.owner == owner.as_str()) {
            slot.take().unwrap().cancel.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    async fn delete_profile(
        &self,
        user: String,
        id: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        self.authorize_manage(conn, &hdr, &user).await?;
        let mut us = self.load(&user)?;
        let before = us.profiles.len();
        us.profiles.retain(|p| p.id != id);
        if us.profiles.len() == before {
            return Err(fdo::Error::InvalidArgs("no such profile".into()));
        }
        self.store.save(&user, &us).map_err(failed)?;
        info!("deleted profile {id} of {user}");
        Ok(())
    }

    async fn rename_profile(
        &self,
        user: String,
        id: String,
        label: String,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        self.authorize_manage(conn, &hdr, &user).await?;
        let mut us = self.load(&user)?;
        let p = us.profiles.iter_mut().find(|p| p.id == id).ok_or_else(|| fdo::Error::InvalidArgs("no such profile".into()))?;
        p.label = label.trim().chars().take(64).collect();
        self.store.save(&user, &us).map_err(failed)
    }

    #[zbus(property)]
    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    #[zbus(property)]
    fn model_id(&self) -> &str {
        &self.model_id
    }

    /// pam-<service> (bool, absent if the file doesn't exist), allow-rgb,
    /// gdm-keyring (pam_gnome_keyring present in gdm-password).
    async fn get_system_settings(&self) -> fdo::Result<HashMap<String, zbus::OwnedVariant>> {
        let mut m = HashMap::new();
        for svc in pamconf::SERVICES {
            if let Some(on) = self.pam.status(svc) {
                m.insert(format!("pam-{svc}"), zbus::OwnedVariant::from(on));
            }
        }
        m.insert("allow-rgb".into(), zbus::OwnedVariant::from(self.cfg.camera.allow_rgb));
        m.insert("gdm-keyring".into(), zbus::OwnedVariant::from(self.pam.gdm_has_keyring()));
        Ok(m)
    }

    /// Turns face authentication on/off for a PAM service (sudo,
    /// gdm-password, polkit-1). Admin authorization required.
    async fn set_pam_enabled(
        &self,
        service: String,
        enabled: bool,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        if !pamconf::SERVICES.contains(&service.as_str()) {
            return Err(fdo::Error::InvalidArgs(format!("unsupported service {service}")));
        }
        self.authorize_configure(conn, &hdr).await?;
        self.pam.set(&service, enabled).map_err(|e| failed(format!("{e:#}")))?;
        info!("face authentication {} for PAM service {service}", if enabled { "enabled" } else { "disabled" });
        Ok(())
    }

    /// Allows/forbids falling back to a colour webcam. Rewrites
    /// config.toml (comments kept) and restarts the daemon to apply it.
    async fn set_allow_rgb(
        &self,
        enabled: bool,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        self.authorize_configure(conn, &hdr).await?;
        let path = self.config_path.clone();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(failed(e)),
        };
        let mut doc: toml_edit::DocumentMut = text.parse().map_err(failed)?;
        if !doc.contains_table("camera") {
            doc["camera"] = toml_edit::table();
        }
        doc["camera"]["allow_rgb"] = toml_edit::value(enabled);
        let tmp = path.with_extension("toml.facelock-tmp");
        std::fs::write(&tmp, doc.to_string()).map_err(failed)?;
        std::fs::rename(&tmp, &path).map_err(failed)?;
        info!("allow_rgb set to {enabled}; restarting to apply");
        // Exit shortly after replying; D-Bus activation starts us again
        // with the new configuration on the next call.
        tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            std::process::exit(0);
        });
        Ok(())
    }

    /// Human-readable camera name ("" if none found).
    #[zbus(property)]
    fn camera_name(&self) -> String {
        self.camera.lock().unwrap().as_ref().map(|c| c.name.clone()).unwrap_or_default()
    }

    /// "ir", "rgb", or "" when no usable camera was found.
    #[zbus(property)]
    fn camera_kind(&self) -> String {
        self.camera.lock().unwrap().as_ref().map(|c| c.kind.code().to_string()).unwrap_or_default()
    }

    #[zbus(property)]
    fn camera_device(&self) -> String {
        self.camera.lock().unwrap().as_ref().map(|c| c.path().display().to_string()).unwrap_or_default()
    }

    /// Similarity a frame needs to count as a match.
    #[zbus(property)]
    fn threshold(&self) -> f64 {
        self.cfg.verify.threshold as f64
    }

    #[zbus(signal)]
    async fn enroll_calibrating(em: &SignalEmitter<'_>, progress: f64) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn enroll_step(
        em: &SignalEmitter<'_>,
        index: u32,
        total: u32,
        instruction: &str,
        axis: &str,
        target: f64,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn enroll_progress(em: &SignalEmitter<'_>, value: f64, off_axis: f64, hint: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn enroll_captured(em: &SignalEmitter<'_>, index: u32) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn enroll_skipped(em: &SignalEmitter<'_>, index: u32) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn enroll_finished(em: &SignalEmitter<'_>, ok: bool, profile_id: &str, error: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn preview_frame(
        em: &SignalEmitter<'_>,
        width: u32,
        height: u32,
        pixels: &[u8],
        face: &[f64],
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn preview_stats(
        em: &SignalEmitter<'_>,
        similarity: f64,
        emitter_gain: f64,
        real_score: f64,
        yaw: f64,
        pitch: f64,
        eyewear: &str,
    ) -> zbus::Result<()>;
}
