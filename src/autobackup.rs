//! One automatic backup a day, kept next to the library, so a bad import or a wrong delete is never the end.
//! It only runs when something changed since the last one, and keeps the newest few.
use crate::{jobs::Job, store::Store};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// How many automatic backups stay on disk.
pub const KEEP: usize = 7;
const EVERY_HOURS: i64 = 22;

#[derive(Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    disabled: bool,
    /// When the last automatic backup finished (RFC 3339).
    #[serde(default)]
    at: String,
    /// What the library looked like then, to tell whether anything changed since.
    #[serde(default)]
    signature: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AutoFile {
    pub name: String,
    pub bytes: u64,
    pub at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AutoInfo {
    pub enabled: bool,
    pub files: Vec<AutoFile>,
}

pub fn valid_name(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix("RecallPort-auto-")
        .and_then(|r| r.strip_suffix(".zip"))
    else {
        return false;
    };
    rest.len() == 13
        && rest.bytes().enumerate().all(|(i, b)| {
            if i == 8 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
}

impl Store {
    pub fn backup_dir(&self) -> PathBuf {
        self.dir.join("backups")
    }

    pub fn backup_path(&self, name: &str) -> Option<PathBuf> {
        valid_name(name)
            .then(|| self.backup_dir().join(name))
            .filter(|p| p.is_file())
    }

    fn auto_state(&self) -> State {
        fs::read(self.backup_dir().join("state.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save_auto_state(&self, state: &State) -> Result<()> {
        let dir = self.backup_dir();
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("state.json"), serde_json::to_vec(state)?)?;
        Ok(())
    }

    pub fn auto_backup_info(&self) -> AutoInfo {
        let mut files: Vec<AutoFile> = fs::read_dir(self.backup_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|item| {
                let name = item.file_name().to_string_lossy().into_owned();
                let meta = item.metadata().ok()?;
                (valid_name(&name) && meta.is_file()).then(|| AutoFile {
                    at: meta
                        .modified()
                        .ok()
                        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
                        .unwrap_or_default(),
                    bytes: meta.len(),
                    name,
                })
            })
            .collect();
        files.sort_by(|a, b| b.name.cmp(&a.name));
        AutoInfo {
            enabled: !self.auto_state().disabled,
            files,
        }
    }

    pub fn set_auto_backup(&self, enabled: bool) -> Result<()> {
        let mut state = self.auto_state();
        state.disabled = !enabled;
        self.save_auto_state(&state)
    }

    /// A short description of the library's content: if it is the same as at the last backup, skip.
    async fn signature(&self) -> Result<String> {
        let entries = self.all().await?;
        let newest = entries
            .iter()
            .map(|e| e.updated.as_str())
            .max()
            .unwrap_or("");
        let idx = self.idx.read().await;
        let entity_newest = idx
            .entities
            .values()
            .map(|e| e.updated.as_str())
            .max()
            .unwrap_or("");
        Ok(format!(
            "{}|{newest}|{}|{entity_newest}|{}",
            entries.len(),
            idx.entities.len(),
            idx.files.len()
        ))
    }

    /// Make the daily backup if it is due. Returns the file name when one was written.
    pub async fn auto_backup(&self) -> Result<Option<String>> {
        let mut state = self.auto_state();
        if state.disabled {
            return Ok(None);
        }
        let due = chrono::DateTime::parse_from_rfc3339(&state.at)
            .map(|at| chrono::Utc::now().signed_duration_since(at).num_hours() >= EVERY_HOURS)
            .unwrap_or(true);
        if !due || self.all().await?.is_empty() {
            return Ok(None);
        }
        let signature = self.signature().await?;
        if signature == state.signature {
            return Ok(None);
        }
        let dir = self.backup_dir();
        fs::create_dir_all(&dir)?;
        let name = format!(
            "RecallPort-auto-{}.zip",
            chrono::Local::now().format("%Y%m%d-%H%M")
        );
        let target = dir.join(&name);
        let partial = dir.join(format!("{name}.part"));
        // Memories, entities and vectors only: the attachment files already sit in the data folder, and
        // copying them every day would multiply their size.
        let result = self
            .export_with(partial.clone(), Job::detached(), false)
            .await;
        if let Err(error) = result {
            let _ = fs::remove_file(&partial);
            return Err(error).context("自动备份未完成");
        }
        fs::rename(&partial, &target)?;
        state.at = chrono::Utc::now().to_rfc3339();
        state.signature = signature;
        self.save_auto_state(&state)?;
        prune(&dir);
        Ok(Some(name))
    }
}

/// Keep only the newest `KEEP` automatic backups, and clear leftovers of an interrupted one.
fn prune(dir: &Path) {
    let mut names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|i| i.file_name().to_string_lossy().into_owned())
        .collect();
    for part in names.iter().filter(|n| n.ends_with(".part")) {
        let _ = fs::remove_file(dir.join(part));
    }
    names.retain(|n| valid_name(n));
    names.sort();
    names.reverse();
    for old in names.into_iter().skip(KEEP) {
        let _ = fs::remove_file(dir.join(old));
    }
}
