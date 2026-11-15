//! pam_facelock.so — asks facelockd to authenticate the user with the IR camera.
//!
//! Usage: `auth sufficient pam_facelock.so [quiet] [debug] [keyring]`
//!
//! `keyring` (login managers only): on success, put the user's sealed login
//! password into PAM_AUTHTOK so a later `pam_gnome_keyring.so` (or
//! pam_kwallet) unlocks the keyring. Use a jump instead of `sufficient` so
//! the stack reaches that module:
//!
//! ```text
//! auth  [success=1 default=ignore]  pam_facelock.so keyring
//! auth  substack                    system-local-login
//! auth  optional                    pam_gnome_keyring.so
//! ```
//!
//! The module never prompts for anything and never fails closed: when the
//! face path is unavailable (SSH session, lid closed, daemon down, no enrolled
//! face) it returns PAM_AUTHINFO_UNAVAIL so the stack falls back to the
//! password. It uses libdbus (synchronous, no threads, no async runtime)
//! because it is loaded into sudo, gdm, polkit-agent-helper, screen lockers…

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::time::Duration;

const PAM_SUCCESS: c_int = 0;
const PAM_SERVICE_ERR: c_int = 3;
const PAM_AUTH_ERR: c_int = 7;
const PAM_AUTHINFO_UNAVAIL: c_int = 9;
const PAM_USER_UNKNOWN: c_int = 10;
const PAM_SERVICE: c_int = 1;
const PAM_RHOST: c_int = 4;
const PAM_CONV: c_int = 5;
const PAM_AUTHTOK: c_int = 6;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;
const PAM_SILENT: c_int = 0x8000;

const BUS_NAME: &str = "org.facelock.Daemon1";
const PATH: &str = "/org/facelock/Daemon1";
/// Covers the daemon's own verify timeout plus D-Bus activation.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);

#[repr(C)]
pub struct PamHandle {
    _private: [u8; 0],
}

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

type ConvFn =
    unsafe extern "C" fn(c_int, *mut *const PamMessage, *mut *mut PamResponse, *mut c_void) -> c_int;

#[repr(C)]
struct PamConv {
    conv: Option<ConvFn>,
    appdata_ptr: *mut c_void,
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_get_user(pamh: *mut PamHandle, user: *mut *const c_char, prompt: *const c_char) -> c_int;
    fn pam_get_item(pamh: *const PamHandle, item_type: c_int, item: *mut *const c_void) -> c_int;
    fn pam_set_item(pamh: *mut PamHandle, item_type: c_int, item: *const c_void) -> c_int;
}

struct Opts {
    quiet: bool,
    debug: bool,
    keyring: bool,
}

struct Pam(*mut PamHandle);

impl Pam {
    fn item_str(&self, item: c_int) -> Option<String> {
        let mut p: *const c_void = std::ptr::null();
        // SAFETY: pam_get_item writes a pointer owned by libpam (or null).
        let rc = unsafe { pam_get_item(self.0, item, &mut p) };
        if rc != PAM_SUCCESS || p.is_null() {
            return None;
        }
        // SAFETY: string items are NUL-terminated C strings.
        Some(unsafe { CStr::from_ptr(p as *const c_char) }.to_string_lossy().into_owned())
    }

    fn user(&self) -> Option<String> {
        let mut u: *const c_char = std::ptr::null();
        // SAFETY: may prompt through the conversation; returns a libpam-owned string.
        let rc = unsafe { pam_get_user(self.0, &mut u, std::ptr::null()) };
        if rc != PAM_SUCCESS || u.is_null() {
            return None;
        }
        Some(unsafe { CStr::from_ptr(u) }.to_string_lossy().into_owned())
    }

    /// Hands the keyring password to the modules further down the stack.
    fn set_authtok(&self, secret: &str, service: &str) {
        let Ok(c) = CString::new(secret) else { return };
        let mut bytes = c.into_bytes_with_nul();
        // SAFETY: libpam copies the string; we wipe our copy right after.
        let rc = unsafe { pam_set_item(self.0, PAM_AUTHTOK, bytes.as_ptr() as *const c_void) };
        zeroize::Zeroize::zeroize(&mut bytes);
        if rc != PAM_SUCCESS {
            log(libc::LOG_WARNING, &format!("{service}: could not set PAM_AUTHTOK"));
        }
    }

    /// Shows a one-way message through the application's conversation function.
    fn say(&self, style: c_int, text: &str) {
        let mut p: *const c_void = std::ptr::null();
        if unsafe { pam_get_item(self.0, PAM_CONV, &mut p) } != PAM_SUCCESS || p.is_null() {
            return;
        }
        // SAFETY: PAM_CONV item is a `struct pam_conv *`.
        let conv = unsafe { &*(p as *const PamConv) };
        let Some(f) = conv.conv else { return };
        let Ok(c) = CString::new(text) else { return };
        let msg = PamMessage { msg_style: style, msg: c.as_ptr() };
        let mut msgp: *const PamMessage = &msg;
        let mut resp: *mut PamResponse = std::ptr::null_mut();
        // SAFETY: standard conversation call with one message; the
        // application allocates `resp` with malloc and we must free it.
        unsafe {
            f(1, &mut msgp, &mut resp, conv.appdata_ptr);
            if !resp.is_null() {
                if !(*resp).resp.is_null() {
                    libc::free((*resp).resp as *mut c_void);
                }
                libc::free(resp as *mut c_void);
            }
        }
    }
}

fn log(prio: c_int, msg: &str) {
    if let Ok(c) = CString::new(format!("pam_facelock: {msg}")) {
        // SAFETY: constant "%s" format with one C string argument.
        unsafe { libc::syslog(libc::LOG_AUTHPRIV | prio, c"%s".as_ptr(), c.as_ptr()) };
    }
}

fn french() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .is_some_and(|v| v.starts_with("fr"))
}

fn tr(fr: bool, en: &'static str, fr_s: &'static str) -> &'static str {
    if fr { fr_s } else { en }
}

fn reason_text(fr: bool, code: &str) -> &'static str {
    match code {
        "no-face" => tr(fr, "no face detected", "aucun visage détecté"),
        "no-match" => tr(fr, "face not recognized", "visage non reconnu"),
        "eyewear-mismatch" => tr(fr, "eye reflections don't match the enrolled face", "reflets incohérents avec le visage enregistré"),
        "eyewear-undecided" => tr(fr, "look straight at the screen", "regarde l'écran bien en face"),
        "low-ir" | "no-ir-strobe" => tr(fr, "IR check failed", "contrôle infrarouge échoué"),
        "spoof" => tr(fr, "looks like a photo or a screen", "ressemble à une photo ou un écran"),
        "cooldown" => tr(fr, "too many failures, try again later", "trop d'échecs, réessaie plus tard"),
        _ => tr(fr, "face authentication failed", "échec de l'authentification faciale"),
    }
}

fn lid_closed() -> bool {
    let Ok(dir) = std::fs::read_dir("/proc/acpi/button/lid") else { return false };
    dir.flatten().any(|e| std::fs::read_to_string(e.path().join("state")).is_ok_and(|s| s.contains("closed")))
}

fn authenticate(pamh: *mut PamHandle, flags: c_int, opts: &Opts) -> c_int {
    let pam = Pam(pamh);
    let silent = opts.quiet || flags & PAM_SILENT != 0;
    let fr = french();
    let service = pam.item_str(PAM_SERVICE).unwrap_or_default();

    let Some(user) = pam.user() else { return PAM_USER_UNKNOWN };

    // Remote logins: the camera is not in front of that user.
    if pam.item_str(PAM_RHOST).is_some_and(|h| !h.is_empty() && h != "localhost")
        || std::env::var_os("SSH_CONNECTION").is_some()
    {
        if opts.debug {
            log(libc::LOG_DEBUG, &format!("{service}: remote session, skipping"));
        }
        return PAM_AUTHINFO_UNAVAIL;
    }
    if lid_closed() {
        if opts.debug {
            log(libc::LOG_DEBUG, &format!("{service}: lid closed, skipping"));
        }
        return PAM_AUTHINFO_UNAVAIL;
    }

    let conn = match dbus::blocking::Connection::new_system() {
        Ok(c) => c,
        Err(e) => {
            log(libc::LOG_WARNING, &format!("system bus unavailable: {e}"));
            return PAM_AUTHINFO_UNAVAIL;
        }
    };
    let proxy = conn.with_proxy(BUS_NAME, PATH, CALL_TIMEOUT);

    // Cheap pre-check so users without a face don't see "look at the camera".
    let profiles: Result<(Vec<(String, String, u64, String, u32, bool)>,), _> =
        proxy.method_call(BUS_NAME, "ListProfiles", (user.as_str(),));
    match profiles {
        Ok((p,)) if p.iter().any(|x| x.5) => {}
        Ok(_) => return PAM_AUTHINFO_UNAVAIL,
        Err(e) => {
            log(libc::LOG_WARNING, &format!("facelockd unavailable: {e}"));
            return PAM_AUTHINFO_UNAVAIL;
        }
    }

    if !silent {
        pam.say(PAM_TEXT_INFO, tr(fr, "Looking for your face…", "Regarde la caméra…"));
    }
    let res: Result<(bool, String), _> = if opts.keyring {
        let r: Result<(bool, String, String), _> = proxy.method_call(BUS_NAME, "VerifyAndRelease", (user.as_str(),));
        r.map(|(ok, reason, secret)| {
            let secret = zeroize::Zeroizing::new(secret);
            if ok && !secret.is_empty() {
                pam.set_authtok(&secret, &service);
            }
            (ok, reason)
        })
    } else {
        proxy.method_call(BUS_NAME, "Verify", (user.as_str(),))
    };
    match res {
        Ok((true, _)) => {
            log(libc::LOG_NOTICE, &format!("{service}: face accepted for {user}"));
            PAM_SUCCESS
        }
        Ok((false, reason)) => {
            log(libc::LOG_NOTICE, &format!("{service}: face rejected for {user}: {reason}"));
            if !silent {
                pam.say(PAM_ERROR_MSG, reason_text(fr, &reason));
            }
            match reason.as_str() {
                "no-profile" | "camera" => PAM_AUTHINFO_UNAVAIL,
                _ => PAM_AUTH_ERR,
            }
        }
        Err(e) => {
            log(libc::LOG_WARNING, &format!("Verify failed: {e}"));
            PAM_AUTHINFO_UNAVAIL
        }
    }
}

fn parse_args(argc: c_int, argv: *const *const c_char) -> Opts {
    let mut o = Opts { quiet: false, debug: false, keyring: false };
    for i in 0..argc.max(0) as usize {
        // SAFETY: libpam passes argc valid C strings.
        let a = unsafe { CStr::from_ptr(*argv.add(i)) }.to_string_lossy();
        match a.as_ref() {
            "quiet" => o.quiet = true,
            "debug" => o.debug = true,
            "keyring" => o.keyring = true,
            other => log(libc::LOG_WARNING, &format!("unknown option {other}")),
        }
    }
    o
}

/// # Safety
/// Called by libpam with a valid handle and argument vector.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_authenticate(
    pamh: *mut PamHandle,
    flags: c_int,
    argc: c_int,
    argv: *const *const c_char,
) -> c_int {
    // Never unwind into C.
    std::panic::catch_unwind(|| {
        let opts = parse_args(argc, argv);
        authenticate(pamh, flags, &opts)
    })
    .unwrap_or(PAM_SERVICE_ERR)
}

/// # Safety
/// Called by libpam; we hold no credentials to establish.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_setcred(
    _pamh: *mut PamHandle,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    PAM_SUCCESS
}
