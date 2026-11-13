//! Enrolled face profiles, one JSON file per user, readable by root only.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::embed::Embedding;
use crate::liveness::Eyewear;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sample {
    /// Pose relative to the user's frontal pose at enrollment.
    pub yaw: f32,
    pub pitch: f32,
    pub embedding: Embedding,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub label: String,
    /// Unix seconds.
    pub created: u64,
    /// Recognizer that produced the embeddings.
    pub model: String,
    /// Eye reflections seen at enrollment; `None` if undecided.
    pub eyewear: Option<Eyewear>,
    pub samples: Vec<Sample>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UserStore {
    pub version: u32,
    pub profiles: Vec<Profile>,
}

pub struct Store {
    dir: PathBuf,
}

/// POSIX-ish portable user names only: no path tricks.
pub fn valid_username(user: &str) -> bool {
    !user.is_empty()
        && user.len() <= 64
        && !user.starts_with('-')
        && !user.starts_with('.')
        && user.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '$'))
}

impl Store {
    pub fn new(dir: &Path) -> Self {
        Self { dir: dir.join("users") }
    }

    fn path(&self, user: &str) -> Result<PathBuf> {
        if !valid_username(user) {
            bail!("invalid user name");
        }
        Ok(self.dir.join(format!("{user}.json")))
    }

    pub fn load(&self, user: &str) -> Result<UserStore> {
        let path = self.path(user)?;
        match std::fs::read(&path) {
            Ok(data) => serde_json::from_slice(&data).with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(UserStore { version: 1, profiles: vec![] }),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Atomic replace: write a 0600 temp file, fsync, rename.
    pub fn save(&self, user: &str, store: &UserStore) -> Result<()> {
        let path = self.path(user)?;
        std::fs::create_dir_all(&self.dir)?;
        std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))?;
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
            f.write_all(&serde_json::to_vec(store)?)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

pub fn new_id() -> String {
    let b: [u8; 8] = rand::random();
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
