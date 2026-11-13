//! Login keyring secret: verified against /etc/shadow, sealed with the TPM
//! through `systemd-creds`, released only to root after a face match.
//!
//! The sealed file is useless on another machine or another boot chain
//! (TPM2, bound to PCR 7 = Secure Boot state by default). Root on the running
//! machine can unseal it: that is the price of unlocking the keyring with a
//! face, and why this is opt-in.

use std::ffi::{CString, c_char, c_int, c_void};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use facelock_core::store::valid_username;
use zeroize::{Zeroize, Zeroizing};

#[link(name = "crypt")]
unsafe extern "C" {
    fn crypt_rn(phrase: *const c_char, setting: *const c_char, data: *mut c_void, size: c_int) -> *mut c_char;
}

const CRYPT_DATA_SIZE: usize = 32768;

pub struct Secrets {
    dir: PathBuf,
}

fn cred_name(user: &str) -> String {
    format!("facelock-keyring-{user}")
}

impl Secrets {
    pub fn new(state_dir: &Path) -> Self {
        Self { dir: state_dir.join("secrets") }
    }

    fn path(&self, user: &str) -> Result<PathBuf> {
        if !valid_username(user) {
            bail!("invalid user name");
        }
        Ok(self.dir.join(format!("{user}.cred")))
    }

    pub fn exists(&self, user: &str) -> bool {
        self.path(user).is_ok_and(|p| p.exists())
    }

    pub fn clear(&self, user: &str) -> Result<()> {
        match std::fs::remove_file(self.path(user)?) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    pub fn seal(&self, user: &str, secret: &str) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let path = self.path(user)?;
        std::fs::create_dir_all(&self.dir)?;
        std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))?;
        let mut child = Command::new("systemd-creds")
            .args(["encrypt", "--with-key=tpm2", &format!("--name={}", cred_name(user)), "-"])
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("running systemd-creds")?;
        child.stdin.take().context("stdin")?.write_all(secret.as_bytes())?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            let _ = std::fs::remove_file(&path);
            bail!("systemd-creds encrypt: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(())
    }

    pub fn unseal(&self, user: &str) -> Result<Option<Zeroizing<String>>> {
        let path = self.path(user)?;
        if !path.exists() {
            return Ok(None);
        }
        let out = Command::new("systemd-creds")
            .args(["decrypt", &format!("--name={}", cred_name(user))])
            .arg(&path)
            .arg("-")
            .stdin(Stdio::null())
            .output()
            .context("running systemd-creds")?;
        let mut stdout = Zeroizing::new(out.stdout);
        if !out.status.success() {
            bail!("systemd-creds decrypt: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        let s = String::from_utf8(std::mem::take(&mut *stdout)).context("secret is not UTF-8")?;
        Ok(Some(Zeroizing::new(s)))
    }
}

/// Checks `password` against the user's /etc/shadow hash (any scheme libcrypt
/// knows: yescrypt, sha512…). Locked or password-less accounts never match.
pub fn verify_password(user: &str, password: &str) -> Result<bool> {
    let shadow = Zeroizing::new(std::fs::read_to_string("/etc/shadow").context("reading /etc/shadow")?);
    let Some(hash) = shadow
        .lines()
        .find_map(|l| l.split_once(':').filter(|(u, _)| *u == user).map(|(_, rest)| rest.split(':').next().unwrap_or("")))
    else {
        bail!("user not in /etc/shadow");
    };
    if hash.is_empty() || hash.starts_with('!') || hash.starts_with('*') {
        return Ok(false);
    }
    let phrase = Zeroizing::new(CString::new(password).context("NUL in password")?.into_bytes_with_nul());
    let setting = CString::new(hash)?;
    let mut data = Zeroizing::new(vec![0u8; CRYPT_DATA_SIZE]);
    // SAFETY: valid NUL-terminated inputs; `data` is a zeroed crypt_data-sized buffer.
    let out = unsafe {
        crypt_rn(phrase.as_ptr() as *const c_char, setting.as_ptr(), data.as_mut_ptr() as *mut c_void, CRYPT_DATA_SIZE as c_int)
    };
    if out.is_null() {
        return Ok(false);
    }
    // SAFETY: crypt_rn returns a NUL-terminated string inside `data`.
    let computed = unsafe { std::ffi::CStr::from_ptr(out) }.to_bytes().to_vec();
    let expected = hash.as_bytes();
    // Constant-time comparison.
    let mut diff = (computed.len() ^ expected.len()) as u8;
    for (a, b) in computed.iter().zip(expected) {
        diff |= a ^ b;
    }
    let mut computed = computed;
    computed.zeroize();
    Ok(diff == 0)
}
