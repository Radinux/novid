//! Adds/removes facelock in /etc/pam.d service files.
//!
//! Only lines we own are touched: each is preceded by a `# facelock:` marker
//! comment, and removal also drops any bare `pam_facelock.so` line (e.g. one
//! added by hand). Everything else is kept byte for byte.
//!
//! * sudo, polkit-1: `auth sufficient pam_facelock.so` before the first auth
//!   line.
//! * gdm-password with pam_gnome_keyring: a jump over the password substack
//!   so the keyring module still runs and receives the sealed password:
//!   `auth [success=N default=ignore] pam_facelock.so keyring`, and the
//!   `include system-local-login` becomes `substack` (jumps cannot skip over
//!   an `include` as a unit). That rewrite is marked and reverted on removal.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub const SERVICES: [&str; 3] = ["sudo", "gdm-password", "polkit-1"];

const MARK: &str = "# facelock: face authentication (managed by facelock, do not edit)";
const MARK_SUBSTACK: &str = "# facelock: include changed to substack (reverted when facelock is disabled)";

fn is_auth(line: &str) -> bool {
    let t = line.trim_start();
    !t.starts_with('#') && matches!(t.split_whitespace().next(), Some("auth" | "-auth"))
}

fn is_ours(line: &str) -> bool {
    let t = line.trim_start();
    !t.starts_with('#') && t.split_whitespace().any(|w| w == "pam_facelock.so" || w.ends_with("/pam_facelock.so"))
}

pub fn is_enabled(content: &str) -> bool {
    content.lines().any(|l| is_auth(l) && is_ours(l))
}

/// Removes everything facelock added, reverting the substack rewrite.
pub fn disable(content: &str) -> String {
    let mut out = Vec::new();
    let mut revert_next = false;
    for line in content.lines() {
        if line.trim() == MARK || is_ours(line) {
            continue;
        }
        if line.trim() == MARK_SUBSTACK {
            revert_next = true;
            continue;
        }
        if revert_next {
            revert_next = false;
            out.push(line.replacen("substack", "include", 1));
            continue;
        }
        out.push(line.to_string());
    }
    let mut s = out.join("\n");
    if content.ends_with('\n') {
        s.push('\n');
    }
    s
}

pub fn enable(content: &str, service: &str) -> Result<String> {
    let base = disable(content);
    let lines: Vec<&str> = base.lines().collect();
    let Some(first_auth) = lines.iter().position(|l| is_auth(l)) else {
        bail!("no auth line in {service}");
    };
    let has_keyring = lines.iter().any(|l| is_auth(l) && l.contains("pam_gnome_keyring.so"));

    let mut out: Vec<String> = lines[..first_auth].iter().map(|s| s.to_string()).collect();
    if service == "gdm-password" && has_keyring {
        // The password path: first include/substack of an auth stack.
        let Some(stack) = (first_auth..lines.len()).find(|&i| {
            is_auth(lines[i]) && matches!(lines[i].split_whitespace().nth(1), Some("include" | "substack"))
        }) else {
            bail!("gdm-password: no auth include/substack line found");
        };
        // Jump over every auth module from here to the password substack.
        let skip = (first_auth..=stack).filter(|&i| is_auth(lines[i])).count();
        out.push(MARK.into());
        out.push(format!("auth       [success={skip} default=ignore]  pam_facelock.so keyring"));
        for (i, l) in lines.iter().enumerate().skip(first_auth) {
            if i == stack && l.split_whitespace().nth(1) == Some("include") {
                out.push(MARK_SUBSTACK.into());
                out.push(l.replacen("include", "substack", 1));
            } else {
                out.push(l.to_string());
            }
        }
    } else {
        out.push(MARK.into());
        out.push("auth       sufficient   pam_facelock.so".into());
        out.extend(lines[first_auth..].iter().map(|s| s.to_string()));
    }
    let mut s = out.join("\n");
    s.push('\n');
    Ok(s)
}

pub struct PamFiles {
    dir: PathBuf,
    backups: PathBuf,
}

impl PamFiles {
    pub fn new(state_dir: &Path) -> Self {
        Self { dir: "/etc/pam.d".into(), backups: state_dir.join("pam-backup") }
    }

    fn path(&self, service: &str) -> Result<PathBuf> {
        if !SERVICES.contains(&service) {
            bail!("unsupported PAM service {service}");
        }
        Ok(self.dir.join(service))
    }

    pub fn status(&self, service: &str) -> Option<bool> {
        let p = self.path(service).ok()?;
        std::fs::read_to_string(p).ok().map(|c| is_enabled(&c))
    }

    pub fn set(&self, service: &str, enabled: bool) -> Result<()> {
        let path = self.path(service)?;
        let content = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let new = if enabled { enable(&content, service)? } else { disable(&content) };
        if new == content {
            return Ok(());
        }
        // Keep the pristine original once, before our first change.
        std::fs::create_dir_all(&self.backups)?;
        let backup = self.backups.join(service);
        if !backup.exists() {
            std::fs::write(&backup, &content)?;
        }
        let tmp = path.with_extension("facelock-tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(new.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn gdm_has_keyring(&self) -> bool {
        std::fs::read_to_string(self.dir.join("gdm-password"))
            .is_ok_and(|c| c.lines().any(|l| is_auth(l) && l.contains("pam_gnome_keyring.so")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUDO: &str = "auth sufficient pam_facelock.so\nauth sufficient pam_howdy.so\n\n#%PAM-1.0\nauth\t\tinclude\t\tsystem-auth\naccount\t\tinclude\t\tsystem-auth\nsession\t\tinclude\t\tsystem-auth\nsession\t\toptional\tpam_systemd.so class=none\n";
    const POLKIT: &str = "#%PAM-1.0\n\nauth      sufficient pam_howdy.so\nauth      include    system-auth\naccount   include    system-auth\npassword  include    system-auth\nsession   include    system-auth\n";
    const GDM: &str = "#%PAM-1.0\nauth sufficient pam_howdy.so\nauth       include                     system-local-login\nauth       optional                    pam_gnome_keyring.so\n\naccount    include                     system-local-login\n\npassword   include                     system-local-login\npassword   optional                    pam_gnome_keyring.so use_authtok\n\nsession    include                     system-local-login\nsession    optional                    pam_gnome_keyring.so auto_start\n";

    #[test]
    fn sudo_hand_added_line_is_adopted() {
        assert!(is_enabled(SUDO));
        let off = disable(SUDO);
        assert!(!is_enabled(&off));
        assert!(off.starts_with("auth sufficient pam_howdy.so\n"));
        let on = enable(&off, "sudo").unwrap();
        assert!(on.starts_with(&format!("{MARK}\nauth       sufficient   pam_facelock.so\nauth sufficient pam_howdy.so\n")));
        assert_eq!(disable(&on), off);
    }

    #[test]
    fn polkit_roundtrip() {
        let on = enable(POLKIT, "polkit-1").unwrap();
        assert!(on.contains(&format!("\n{MARK}\nauth       sufficient   pam_facelock.so\nauth      sufficient pam_howdy.so\n")));
        assert_eq!(disable(&on), POLKIT);
        // Idempotent.
        assert_eq!(enable(&on, "polkit-1").unwrap(), on);
    }

    #[test]
    fn gdm_jumps_to_keyring() {
        let on = enable(GDM, "gdm-password").unwrap();
        let auth: Vec<&str> = on.lines().filter(|l| is_auth(l)).collect();
        assert_eq!(auth[0], "auth       [success=2 default=ignore]  pam_facelock.so keyring");
        assert_eq!(auth[1], "auth sufficient pam_howdy.so");
        assert_eq!(auth[2], "auth       substack                     system-local-login");
        assert!(auth[3].contains("pam_gnome_keyring.so"));
        // Other stacks untouched.
        assert!(on.contains("account    include                     system-local-login"));
        assert_eq!(disable(&on), GDM);
    }
}
