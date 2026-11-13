//! Adds/removes facelock in /etc/pam.d service files.
//!
//! Only lines we own are touched: each is preceded by a `# facelock:` marker
//! comment, and removal also drops any bare `pam_facelock.so` line (e.g. one
//! added by hand). Everything else is kept byte for byte.
//!
//! The facelock line goes right before the password stack: `auth include
//! system-auth` / `system-local-login` (Arch) or `@include common-auth`
//! (Debian), so checks placed ahead of it (nologin, root) still run first.
//!
//! * sudo, polkit-1: `auth sufficient pam_facelock.so`.
//! * gdm-password with pam_gnome_keyring: a jump over the password stack so
//!   the keyring module still runs and receives the sealed password:
//!   `auth [success=1 default=ignore] pam_facelock.so keyring`. The stack
//!   becomes a `substack` (jumps cannot skip over an `include` as a unit);
//!   that rewrite is marked and reverted on removal.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub const SERVICES: [&str; 3] = ["sudo", "gdm-password", "polkit-1"];

const MARK: &str = "# facelock: face authentication (managed by facelock, do not edit)";
const MARK_SUBSTACK: &str = "# facelock: include changed to substack (reverted when facelock is disabled)";
const MARK_AT_INCLUDE: &str = "# facelock: @include changed to substack (reverted when facelock is disabled)";

fn is_auth(line: &str) -> bool {
    let t = line.trim_start();
    !t.starts_with('#') && matches!(t.split_whitespace().next(), Some("auth" | "-auth"))
}

fn is_ours(line: &str) -> bool {
    let t = line.trim_start();
    !t.starts_with('#') && t.split_whitespace().any(|w| w == "pam_facelock.so" || w.ends_with("/pam_facelock.so"))
}

/// Debian's `@include common-auth`, which is not an `auth` line itself.
fn is_at_include_auth(line: &str) -> bool {
    let mut words = line.split_whitespace();
    words.next() == Some("@include") && words.next() == Some("common-auth")
}

/// The password stack: an auth include/substack, or Debian's common-auth.
fn is_auth_stack(line: &str) -> bool {
    (is_auth(line) && matches!(line.split_whitespace().nth(1), Some("include" | "substack"))) || is_at_include_auth(line)
}

pub fn is_enabled(content: &str) -> bool {
    content.lines().any(|l| is_auth(l) && is_ours(l))
}

/// Removes everything facelock added, reverting the substack rewrite.
pub fn disable(content: &str) -> String {
    let mut out = Vec::new();
    let mut revert_next: Option<&str> = None;
    for line in content.lines() {
        if line.trim() == MARK || is_ours(line) {
            continue;
        }
        if line.trim() == MARK_SUBSTACK || line.trim() == MARK_AT_INCLUDE {
            revert_next = Some(line.trim());
            continue;
        }
        match revert_next.take() {
            Some(MARK_SUBSTACK) => out.push(line.replacen("substack", "include", 1)),
            Some(_) => out.push(format!("@include {}", line.split_whitespace().last().unwrap_or("common-auth"))),
            None => out.push(line.to_string()),
        }
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
    let Some(at) = lines.iter().position(|l| is_auth_stack(l)).or_else(|| lines.iter().position(|l| is_auth(l)))
    else {
        bail!("no auth line in {service}");
    };
    let has_keyring = lines.iter().any(|l| is_auth(l) && l.contains("pam_gnome_keyring.so"));

    let mut out: Vec<String> = lines[..at].iter().map(|s| s.to_string()).collect();
    out.push(MARK.into());
    if service == "gdm-password" && has_keyring {
        let stack = lines[at];
        if !is_auth_stack(stack) {
            bail!("gdm-password: no auth include/substack line found");
        }
        // Jump over the password stack, landing on the keyring module.
        out.push("auth       [success=1 default=ignore]  pam_facelock.so keyring".into());
        if is_at_include_auth(stack) {
            out.push(MARK_AT_INCLUDE.into());
            out.push("auth       substack                    common-auth".into());
        } else if stack.split_whitespace().nth(1) == Some("include") {
            out.push(MARK_SUBSTACK.into());
            out.push(stack.replacen("include", "substack", 1));
        } else {
            out.push(stack.to_string());
        }
        out.extend(lines[at + 1..].iter().map(|s| s.to_string()));
    } else {
        out.push("auth       sufficient   pam_facelock.so".into());
        out.extend(lines[at..].iter().map(|s| s.to_string()));
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
        assert!(on.contains(&format!("#%PAM-1.0\n{MARK}\nauth       sufficient   pam_facelock.so\nauth\t\tinclude\t\tsystem-auth\n")));
        assert_eq!(disable(&on), off);
    }

    #[test]
    fn polkit_roundtrip() {
        let on = enable(POLKIT, "polkit-1").unwrap();
        assert!(on.contains(&format!("pam_howdy.so\n{MARK}\nauth       sufficient   pam_facelock.so\nauth      include    system-auth\n")));
        assert_eq!(disable(&on), POLKIT);
        // Idempotent.
        assert_eq!(enable(&on, "polkit-1").unwrap(), on);
    }

    #[test]
    fn gdm_jumps_to_keyring() {
        let on = enable(GDM, "gdm-password").unwrap();
        let auth: Vec<&str> = on.lines().filter(|l| is_auth(l)).collect();
        assert_eq!(auth[0], "auth sufficient pam_howdy.so");
        assert_eq!(auth[1], "auth       [success=1 default=ignore]  pam_facelock.so keyring");
        assert_eq!(auth[2], "auth       substack                     system-local-login");
        assert!(auth[3].contains("pam_gnome_keyring.so"));
        // Other stacks untouched.
        assert!(on.contains("account    include                     system-local-login"));
        assert_eq!(disable(&on), GDM);
    }

    // Debian 13 stock files.
    const DEB_SUDO: &str = "#%PAM-1.0\n\n# Set up user limits from /etc/security/limits.conf.\nsession    required   pam_limits.so\n\n@include common-auth\n@include common-account\n@include common-session-noninteractive\n";
    const DEB_GDM: &str = "#%PAM-1.0\nauth    requisite       pam_nologin.so\nauth\trequired\tpam_succeed_if.so user != root quiet_success\n@include common-auth\nauth    optional        pam_gnome_keyring.so\n@include common-account\nsession required        pam_loginuid.so\nsession optional        pam_gnome_keyring.so auto_start\n@include common-password\n";

    #[test]
    fn debian_sudo_roundtrip() {
        let on = enable(DEB_SUDO, "sudo").unwrap();
        assert!(on.contains(&format!("\n{MARK}\nauth       sufficient   pam_facelock.so\n@include common-auth\n")));
        assert!(is_enabled(&on));
        assert_eq!(disable(&on), DEB_SUDO);
        assert_eq!(enable(&on, "sudo").unwrap(), on);
    }

    #[test]
    fn debian_gdm_jumps_to_keyring() {
        let on = enable(DEB_GDM, "gdm-password").unwrap();
        let auth: Vec<&str> = on.lines().filter(|l| is_auth(l)).collect();
        // nologin and the root check still run before the face.
        assert!(auth[0].contains("pam_nologin.so"));
        assert!(auth[1].contains("pam_succeed_if.so"));
        assert_eq!(auth[2], "auth       [success=1 default=ignore]  pam_facelock.so keyring");
        assert_eq!(auth[3], "auth       substack                    common-auth");
        assert!(auth[4].contains("pam_gnome_keyring.so"));
        assert!(!on.contains("@include common-auth"));
        assert!(on.contains("@include common-account"));
        assert_eq!(disable(&on), DEB_GDM);
        assert_eq!(enable(&on, "gdm-password").unwrap(), on);
    }
}
