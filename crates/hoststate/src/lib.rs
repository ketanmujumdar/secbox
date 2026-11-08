//! The state folder: where it is, how it is created, and project metadata.
//!
//! The folder is always named [`brand::STATE_DIR`] and sits directly in the
//! home folder or its override, so no setting can point it at another name.
//! Secret values are never written here.
//!
//! [`host`] sits here too, because the daemon and every client need the same
//! reading of what the machine has.

pub mod host;

use std::ffi::OsString;
use std::fs::{self, DirBuilder, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use secrets::{AssignmentSet, AuditAction, AuditEvent, ProjectId, SecretName};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Written when this tool creates the state folder. Uninstall refuses a folder
/// without it, so a folder of the same name from another tool stays safe.
pub const MARKER_FILE: &str = ".state-dir-marker";
/// The engine's state folder, inside the state folder. It is kept short because
/// the engine's socket paths are built from it. The engine can create this
/// before anything here has run, so a state folder holding only this counts as
/// ours even with no marker yet.
pub const ENGINE_DIR: &str = "msb";
const META_FILE: &str = "secrets.json";
const AUDIT_FILE: &str = "audit.jsonl";
/// The daemon's own record of what it did and refused, one JSON entry a line.
/// The daemon writes it; a client on this machine may read it.
pub const DAEMON_AUDIT_FILE: &str = "daemon-audit.jsonl";
const PROJECTS_DIR: &str = "projects";
const MAX_META_BYTES: u64 = 1024 * 1024;
const META_VERSION: u32 = 1;

pub fn home_override_var() -> String {
    format!("{}{}", brand::ENV_PREFIX, brand::HOME_ENV_SUFFIX)
}

pub fn engine_bin_var() -> String {
    format!("{}{}", brand::ENV_PREFIX, brand::ENGINE_BIN_ENV_SUFFIX)
}

#[derive(Debug, Clone)]
pub struct StatePaths {
    pub state: PathBuf,
    pub overridden: bool,
}

impl StatePaths {
    /// Works out the path. Touches nothing on disk.
    pub fn locate(env: &dyn Fn(&str) -> Option<OsString>) -> Result<Self, String> {
        let var = home_override_var();
        let (home, source, overridden) = match env(&var) {
            Some(value) => (value, var, true),
            None => match env("HOME") {
                Some(value) => (value, "HOME".to_string(), false),
                None => return Err("HOME is not set".to_string()),
            },
        };
        let home = PathBuf::from(home);
        if !home.is_absolute() {
            return Err(format!("{source} must be an absolute path"));
        }
        if home
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(format!("{source} must not contain . or .. segments"));
        }
        Ok(Self {
            state: home.join(brand::STATE_DIR),
            overridden,
        })
    }

    pub fn projects(&self) -> PathBuf {
        self.state.join(PROJECTS_DIR)
    }

    pub fn project(&self, id: &ProjectId) -> PathBuf {
        self.projects().join(id.as_str())
    }

    pub fn daemon_audit(&self) -> PathBuf {
        self.state.join(DAEMON_AUDIT_FILE)
    }
}

/// What is at the state path right now.
pub enum StateCheck {
    Missing,
    Foreign(String),
    Ours(Metadata),
}

/// Inspects the state path without following a link at it.
pub fn inspect_state(state: &Path) -> Result<StateCheck, String> {
    let meta = match fs::symlink_metadata(state) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(StateCheck::Missing),
        Err(e) => return Err(format!("cannot inspect it: {}", e.kind())),
    };
    if meta.file_type().is_symlink() {
        return Ok(StateCheck::Foreign("it is a symbolic link".to_string()));
    }
    if !meta.is_dir() {
        return Ok(StateCheck::Foreign("it is not a directory".to_string()));
    }
    let marker = fs::symlink_metadata(state.join(MARKER_FILE));
    if marker.is_ok_and(|m| m.is_file()) || holds_only_engine_state(state)? {
        return Ok(StateCheck::Ours(meta));
    }
    Ok(StateCheck::Foreign(format!(
        "it has no {MARKER_FILE} file and holds other files, so this tool did not create it"
    )))
}

fn holds_only_engine_state(state: &Path) -> Result<bool, String> {
    let entries = fs::read_dir(state).map_err(|e| format!("cannot list it: {}", e.kind()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot list it: {}", e.kind()))?;
        let is_real_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if entry.file_name() != ENGINE_DIR || !is_real_dir {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn same_directory(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// Creates the state folder if needed, owner-only, and refuses anything else.
pub fn ensure_state(paths: &StatePaths) -> Result<(), String> {
    match inspect_state(&paths.state)? {
        StateCheck::Foreign(reason) => Err(format!("refusing to use the state folder: {reason}")),
        StateCheck::Ours(meta) => {
            if meta.mode() & 0o077 != 0 {
                fs::set_permissions(&paths.state, fs::Permissions::from_mode(0o700))
                    .map_err(|e| format!("cannot tighten the state folder: {}", e.kind()))?;
            }
            let marker = paths.state.join(MARKER_FILE);
            if fs::symlink_metadata(&marker).is_err() {
                write_marker(&marker)?;
            }
            Ok(())
        }
        StateCheck::Missing => {
            DirBuilder::new()
                .mode(0o700)
                .create(&paths.state)
                .map_err(|e| format!("cannot create the state folder: {}", e.kind()))?;
            write_marker(&paths.state.join(MARKER_FILE))
        }
    }
}

fn write_marker(path: &Path) -> Result<(), String> {
    write_private(
        path,
        format!("{} state folder\n", brand::PRODUCT_NAME).as_bytes(),
    )
}

pub fn ensure_private_dir(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err("a state subfolder is not a real directory".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|e| format!("cannot create a state subfolder: {}", e.kind())),
        Err(e) => Err(format!("cannot inspect a state subfolder: {}", e.kind())),
    }
}

/// Writes through a new owner-only file and a rename, so that a link placed at
/// the destination is replaced and never followed.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Unique per call as well as per process, because the daemon's threads can
    // write the same file at once. The last rename wins, and each is whole.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let call = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = path.with_file_name(format!(".{name}.{}.{call}.tmp", std::process::id()));
    let failure = |what: &str, e: std::io::Error| format!("cannot {what} {name}: {}", e.kind());
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
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

/// A stable id for a project: its folder name plus a hash of its full path.
/// Moving a project gives it a new id, so it does not keep old secrets.
pub fn project_id(canonical: &Path) -> Result<ProjectId, String> {
    let digest = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    let hash: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    let base = canonical
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let clean: String = base
        .chars()
        .take(40)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let id = if clean.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        format!("{clean}-{hash}")
    } else {
        format!("p{clean}-{hash}")
    };
    ProjectId::new(id).map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretSource {
    Prompt,
    Import,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretRecord {
    pub name: SecretName,
    pub source: SecretSource,
    pub updated_unix: u64,
}

/// Non-secret facts about one project's secrets.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectMeta {
    pub version: u32,
    pub project_path: String,
    pub secrets: Vec<SecretRecord>,
    pub assignments: AssignmentSet,
    /// Names the project expects but that have no value yet, such as those
    /// read from a `.env.example`. Nothing is in the keychain for them. Left
    /// out of the file when empty, so metadata without any stays readable by
    /// earlier versions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<SecretName>,
}

impl ProjectMeta {
    pub fn new(project_path: &Path) -> Self {
        Self {
            version: META_VERSION,
            project_path: project_path.to_string_lossy().into_owned(),
            ..Self::default()
        }
    }

    pub fn has(&self, name: &SecretName) -> bool {
        self.secrets.iter().any(|r| &r.name == name)
    }

    /// True if the name waits for a value and has none stored.
    pub fn is_pending(&self, name: &SecretName) -> bool {
        self.pending.contains(name)
    }

    /// Notes a name that needs a value. A name that already has one is left
    /// alone, so a template cannot hide a stored secret. Returns true if added.
    pub fn add_pending(&mut self, name: &SecretName) -> bool {
        if self.has(name) || self.is_pending(name) {
            return false;
        }
        self.pending.push(name.clone());
        self.pending.sort();
        true
    }

    pub fn record(&mut self, name: &SecretName, source: SecretSource) {
        self.pending.retain(|n| n != name);
        self.secrets.retain(|r| &r.name != name);
        self.secrets.push(SecretRecord {
            name: name.clone(),
            source,
            updated_unix: now_unix(),
        });
        self.secrets.sort_by(|a, b| a.name.cmp(&b.name));
    }
}

/// A note that this project exists, and where it was last seen.
///
/// Written whenever a project is opened, so that a client can list the
/// projects this machine knows about without being told about them. It holds
/// no secret and nothing from inside a sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectRecord {
    pub version: u32,
    /// The canonical path, as the host resolved it.
    pub path: String,
    pub first_seen_unix: u64,
    pub last_seen_unix: u64,
}

const PROJECT_FILE: &str = "project.json";

impl ProjectRecord {
    pub fn load(project_dir: &Path) -> Result<Option<Self>, String> {
        let path = project_dir.join(PROJECT_FILE);
        let Some(text) = read_limited(&path, MAX_META_BYTES)? else {
            return Ok(None);
        };
        let record: Self = serde_json::from_str(&text)
            .map_err(|_| "the project record is damaged or from another version".to_string())?;
        if record.version != META_VERSION {
            return Err("the project record is from another version".to_string());
        }
        Ok(Some(record))
    }

    /// Records this project at this path, keeping the time it was first seen.
    pub fn touch(project_dir: &Path, canonical: &Path, now_unix: u64) -> Result<Self, String> {
        let first_seen_unix = Self::load(project_dir)?.map_or(now_unix, |old| old.first_seen_unix);
        let record = Self {
            version: META_VERSION,
            path: canonical.to_string_lossy().into_owned(),
            first_seen_unix,
            last_seen_unix: now_unix,
        };
        let text =
            serde_json::to_vec_pretty(&record).map_err(|_| "cannot encode the record".to_string())?;
        write_private(&project_dir.join(PROJECT_FILE), &text)?;
        Ok(record)
    }
}

/// Every project this machine knows about, by identity, most recently seen
/// first. A folder with no readable record is left out rather than guessed at.
pub fn known_projects(paths: &StatePaths) -> Vec<(ProjectId, ProjectRecord)> {
    let Ok(entries) = fs::read_dir(paths.projects()) else {
        return Vec::new();
    };
    let mut found: Vec<(ProjectId, ProjectRecord)> = Vec::new();
    for entry in entries.flatten().take(10_000) {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(id) = ProjectId::new(entry.file_name().to_string_lossy().into_owned()) else {
            continue;
        };
        if let Ok(Some(record)) = ProjectRecord::load(&entry.path()) {
            found.push((id, record));
        }
    }
    found.sort_by(|a, b| {
        b.1.last_seen_unix
            .cmp(&a.1.last_seen_unix)
            .then_with(|| a.0.cmp(&b.0))
    });
    found
}

/// Reads a small regular file, or `None` when there is none.
fn read_limited(path: &Path, limit: u64) -> Result<Option<String>, String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Err("a state file is not a regular file".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot inspect a state file: {}", e.kind())),
    }
    let file = fs::File::open(path).map_err(|e| format!("cannot open it: {}", e.kind()))?;
    let mut text = String::new();
    file.take(limit)
        .read_to_string(&mut text)
        .map_err(|e| format!("cannot read it: {}", e.kind()))?;
    Ok(Some(text))
}

/// Reads one metadata file. A missing file is `None`.
pub fn read_meta(project_dir: &Path) -> Result<Option<ProjectMeta>, String> {
    let path = project_dir.join(META_FILE);
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Err("the secrets metadata is not a regular file".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot inspect the metadata: {}", e.kind())),
    }
    let file = fs::File::open(&path).map_err(|e| format!("cannot open it: {}", e.kind()))?;
    let mut text = String::new();
    file.take(MAX_META_BYTES)
        .read_to_string(&mut text)
        .map_err(|e| format!("cannot read the metadata: {}", e.kind()))?;
    let meta: ProjectMeta = serde_json::from_str(&text)
        .map_err(|_| "the secrets metadata is damaged or from another version".to_string())?;
    if meta.version != META_VERSION {
        return Err("the secrets metadata is from another version".to_string());
    }
    Ok(Some(meta))
}

/// Creates the folders for a project, so that a failure comes before a secret
/// is stored rather than after.
pub fn prepare_project(paths: &StatePaths, id: &ProjectId) -> Result<PathBuf, String> {
    ensure_state(paths)?;
    ensure_private_dir(&paths.projects())?;
    let dir = paths.project(id);
    ensure_private_dir(&dir)?;
    Ok(dir)
}

pub fn write_meta(project_dir: &Path, meta: &ProjectMeta) -> Result<(), String> {
    let text = serde_json::to_vec_pretty(meta)
        .map_err(|_| "cannot encode the secrets metadata".to_string())?;
    write_private(&project_dir.join(META_FILE), &text)
}

/// Appends a names-only audit record.
pub fn audit(project_dir: &Path, action: AuditAction, name: &SecretName) -> Result<(), String> {
    let path = project_dir.join(AUDIT_FILE);
    if fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file()) {
        return Err("the audit log is not a regular file".to_string());
    }
    let event = AuditEvent::new(now_unix(), action, name.clone(), None);
    let mut line =
        serde_json::to_vec(&event).map_err(|_| "cannot encode an audit record".to_string())?;
    line.push(b'\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut file| file.write_all(&line))
        .map_err(|e| format!("cannot write the audit log: {}", e.kind()))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    #[test]
    fn many_threads_can_write_the_same_file_at_once() {
        let dir = std::env::temp_dir().join(format!("write-private-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("project.json");
        let workers: Vec<_> = (0..16)
            .map(|worker| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        write_private(&path, format!("{worker}").as_bytes()).unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        // Whole, from one writer, with no temporary file left behind.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.parse::<u32>().unwrap() < 16);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    use super::*;

    #[test]
    fn project_ids_are_valid_and_differ_by_path() {
        let a = project_id(Path::new("/x/My Project!")).unwrap();
        let b = project_id(Path::new("/y/My Project!")).unwrap();
        assert_ne!(a, b);
        assert!(a.as_str().starts_with("My-Project--"));
        let hidden = project_id(Path::new("/x/.hidden")).unwrap();
        assert!(hidden.as_str().starts_with("p.hidden-"));
        assert!(project_id(Path::new("/")).is_ok());
    }

    #[test]
    fn a_project_record_keeps_when_it_was_first_seen() {
        let dir = std::env::temp_dir().join(format!("rec-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        DirBuilder::new().mode(0o700).create(&dir).unwrap();
        assert_eq!(ProjectRecord::load(&dir).unwrap(), None);
        let first = ProjectRecord::touch(&dir, Path::new("/p/one"), 100).unwrap();
        assert_eq!(first.first_seen_unix, 100);
        let later = ProjectRecord::touch(&dir, Path::new("/p/moved"), 200).unwrap();
        assert_eq!(later.first_seen_unix, 100);
        assert_eq!(later.last_seen_unix, 200);
        assert_eq!(later.path, "/p/moved");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn state_path_cannot_be_redirected_by_dots_or_relative_paths() {
        let var = home_override_var();
        for bad in ["relative/path", "/tmp/a/../..", ""] {
            let env = |key: &str| (key == var).then(|| OsString::from(bad));
            assert!(StatePaths::locate(&env).is_err(), "{bad:?}");
        }
        let env = |key: &str| (key == var).then(|| OsString::from("/tmp/somewhere"));
        let paths = StatePaths::locate(&env).unwrap();
        assert_eq!(
            paths.state,
            Path::new("/tmp/somewhere").join(brand::STATE_DIR)
        );
        assert!(paths.overridden);
    }
}
