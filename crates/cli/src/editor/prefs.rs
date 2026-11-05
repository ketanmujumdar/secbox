//! Which checks are on, and the record of overrides.
//!
//! Both files live in the state folder in the home folder. Nothing in a
//! project folder is read here and no setting can move the file, so a repo
//! cannot turn a check off or hide an override.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::state::{ensure_state, StatePaths};

const PREFS_FILE: &str = "editor-checks.json";
const AUDIT_FILE: &str = "editor-audit.jsonl";
pub const BACKUP_DIR: &str = "editor-backups";
const MAX_PREFS_BYTES: u64 = 64 * 1024;
const VERSION: u32 = 1;

/// The user's choices. Only ids that are turned off are stored, so a check
/// added in a later release starts on.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prefs {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub off: Vec<String>,
}

impl Prefs {
    pub fn is_on(&self, id: &str) -> bool {
        !self.off.iter().any(|other| other == id)
    }

    pub fn set(&mut self, id: &str, on: bool) {
        self.off.retain(|other| other != id);
        if !on {
            self.off.push(id.to_string());
        }
        self.off.sort();
        self.version = VERSION;
    }
}

/// Reads the choices. A missing file means every check is on. A damaged file
/// is an error, because carrying on would silently run the wrong set.
pub fn read(paths: &StatePaths) -> Result<Prefs, String> {
    let path = paths.state.join(PREFS_FILE);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Prefs::default()),
        Err(e) => return Err(format!("cannot inspect the check list: {}", e.kind())),
        Ok(meta) if !meta.is_file() => {
            return Err("the check list is not a regular file".to_string())
        }
        Ok(_) => {}
    }
    let file = fs::File::open(&path).map_err(|e| format!("cannot open it: {}", e.kind()))?;
    let mut text = String::new();
    file.take(MAX_PREFS_BYTES)
        .read_to_string(&mut text)
        .map_err(|e| format!("cannot read the check list: {}", e.kind()))?;
    let prefs: Prefs = serde_json::from_str(&text)
        .map_err(|_| "the check list is damaged or from another version".to_string())?;
    if prefs.version != VERSION {
        return Err("the check list is from another version".to_string());
    }
    Ok(prefs)
}

pub fn write(paths: &StatePaths, prefs: &Prefs) -> Result<(), String> {
    ensure_state(paths)?;
    let text =
        serde_json::to_vec_pretty(prefs).map_err(|_| "cannot encode the check list".to_string())?;
    write_private(&paths.state.join(PREFS_FILE), &text)
}

/// One line of the audit log. It holds a path and check ids, never file
/// contents, so nothing from a hostile repo lands in the log as data to trust.
#[derive(Debug, Serialize)]
pub struct Override {
    pub unix_seconds: u64,
    pub action: &'static str,
    pub editor: &'static str,
    pub project: String,
    pub failed: Vec<String>,
}

/// Appends an override record. The caller must have made the state folder.
pub fn record(paths: &StatePaths, event: &Override) -> Result<(), String> {
    ensure_state(paths)?;
    let path = paths.state.join(AUDIT_FILE);
    if fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file()) {
        return Err("the audit log is not a regular file".to_string());
    }
    let mut line =
        serde_json::to_vec(event).map_err(|_| "cannot encode an audit record".to_string())?;
    line.push(b'\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut file| file.write_all(&line))
        .map_err(|e| format!("cannot write the audit log: {}", e.kind()))
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Writes through a new owner-only file and a rename, so a link placed at the
/// destination is replaced rather than followed.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    write_mode(path, bytes, 0o600)
}

/// The same, keeping a chosen mode. Used when rewriting a file that is not
/// ours, so the editor's own permissions survive.
pub fn write_mode(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let failure = |what: &str, e: std::io::Error| format!("cannot {what} {name}: {}", e.kind());
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temp)
        .map_err(|e| failure("create", e))?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(failure("write", e));
    }
    fs::rename(&temp, path).map_err(|e| {
        let _ = fs::remove_file(&temp);
        failure("replace", e)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turning_a_check_off_and_on_again_leaves_no_trace() {
        let mut prefs = Prefs::default();
        assert!(prefs.is_on("a"));
        prefs.set("a", false);
        prefs.set("a", false);
        assert!(!prefs.is_on("a"));
        assert_eq!(prefs.off, vec!["a".to_string()]);
        prefs.set("a", true);
        assert!(prefs.is_on("a"));
        assert!(prefs.off.is_empty());
    }

    #[test]
    fn a_damaged_check_list_is_an_error_not_an_empty_one() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StatePaths {
            state: dir.path().to_path_buf(),
            overridden: true,
        };
        assert!(read(&paths).unwrap().off.is_empty());
        fs::write(dir.path().join(PREFS_FILE), b"{ not json").unwrap();
        assert!(read(&paths).is_err());
        fs::write(dir.path().join(PREFS_FILE), br#"{"version":99,"off":[]}"#).unwrap();
        assert!(read(&paths).is_err());
    }
}
