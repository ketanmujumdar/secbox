//! Settings a person set, kept on the host.
//!
//! Two kinds, and the difference matters. [`UserSettings`] are the ceilings a
//! person set for themselves; nothing in a project can raise them. The
//! per-project file holds what a person has approved for one project: the
//! ports that may reach `127.0.0.1` and the hosts that sandbox may talk to.
//!
//! None of this is ever read from the project directory. A repository can ask
//! for something, in its own committed config, but asking is not approving, so
//! a hostile file in a project cannot widen anything here.

use std::path::Path;

use projectcheck::resources::{ByteSize, MilliCpus, UserLimits};
use serde::{Deserialize, Serialize};

const SETTINGS_FILE: &str = "settings.json";
const APPROVALS_FILE: &str = "approvals.json";
const VERSION: u32 = 1;
const MAX_FILE_BYTES: u64 = 256 * 1024;

const GIB: u64 = 1 << 30;

/// Defaults chosen from what the engine spike measured: `demo_shop` needed
/// more than 2G to build and peaked at 2.8 GiB, and the host keeps the whole
/// VM's size resident, so the ceiling has to be a size a laptop can spare.
const DEFAULT_MAX_MEMORY: u64 = 8 * GIB;
const DEFAULT_MAX_CPUS_MILLIS: u32 = 8_000;
const DEFAULT_MAX_DISK: u64 = 60 * GIB;
/// Left free for the host. The spike found the host keeps roughly the full VM
/// size resident during and after a build, so this is not generous.
const DEFAULT_MEMORY_RESERVE: u64 = 2 * GIB;
const DEFAULT_DISK_RESERVE: u64 = 10 * GIB;
const DEFAULT_CPU_RESERVE_MILLIS: u32 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserSettings {
    pub version: u32,
    pub max_memory_bytes: u64,
    pub max_cpus_millis: u32,
    pub max_disk_bytes: u64,
    pub max_build_memory_bytes: Option<u64>,
    pub memory_reserve_bytes: u64,
    pub disk_reserve_bytes: u64,
    pub cpu_reserve_millis: u32,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            version: VERSION,
            max_memory_bytes: DEFAULT_MAX_MEMORY,
            max_cpus_millis: DEFAULT_MAX_CPUS_MILLIS,
            max_disk_bytes: DEFAULT_MAX_DISK,
            max_build_memory_bytes: None,
            memory_reserve_bytes: DEFAULT_MEMORY_RESERVE,
            disk_reserve_bytes: DEFAULT_DISK_RESERVE,
            cpu_reserve_millis: DEFAULT_CPU_RESERVE_MILLIS,
        }
    }
}

impl UserSettings {
    /// Reads the file, or returns the defaults when there is none. A damaged
    /// file is an error rather than a silent fall back to the defaults,
    /// because falling back could quietly raise a ceiling the person lowered.
    pub fn load(state: &Path) -> Result<Self, String> {
        match read_json::<Self>(&state.join(SETTINGS_FILE))? {
            None => Ok(Self::default()),
            Some(settings) if settings.version == VERSION => Ok(settings),
            Some(_) => Err("the settings file is from another version".to_owned()),
        }
    }

    pub fn save(&self, state: &Path) -> Result<(), String> {
        write_json(&state.join(SETTINGS_FILE), self)
    }

    /// A zero or out-of-range ceiling is an error rather than a value quietly
    /// replaced, for the same reason a damaged file is.
    pub fn limits(&self) -> Result<UserLimits, String> {
        let size = |bytes, what: &str| {
            ByteSize::new(bytes).ok_or_else(|| format!("the {what} maximum must be above zero"))
        };
        Ok(UserLimits {
            max_memory: size(self.max_memory_bytes, "memory")?,
            max_cpus: MilliCpus::new(self.max_cpus_millis)
                .ok_or_else(|| "the CPU maximum is zero or too large".to_owned())?,
            max_disk: size(self.max_disk_bytes, "disk")?,
            max_build_memory: self
                .max_build_memory_bytes
                .map(|b| size(b, "build memory"))
                .transpose()?,
        })
    }
}

/// What a person has approved for one project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approvals {
    #[serde(default)]
    pub version: u32,
    /// Host ports that may be forwarded to `127.0.0.1`.
    #[serde(default)]
    pub ports: Vec<u16>,
    /// Hostnames this project's sandbox may reach while it runs. Build hosts
    /// are separate and built in; these are the ones a person added.
    #[serde(default)]
    pub run_hosts: Vec<String>,
}

impl Approvals {
    pub fn load(project_dir: &Path) -> Result<Self, String> {
        Ok(read_json::<Self>(&project_dir.join(APPROVALS_FILE))?.unwrap_or_default())
    }

    pub fn save(&self, project_dir: &Path) -> Result<(), String> {
        let mut copy = self.clone();
        copy.version = VERSION;
        copy.ports.sort_unstable();
        copy.ports.dedup();
        copy.run_hosts.sort();
        copy.run_hosts.dedup();
        write_json(&project_dir.join(APPROVALS_FILE), &copy)
    }

    pub fn allows_port(&self, port: u16) -> bool {
        self.ports.contains(&port)
    }
}

/// What a sandbox was actually created with. Written when it is created, so
/// that a later report says what is true of the running sandbox rather than
/// what a fresh plan would ask for today.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxRecord {
    pub version: u32,
    pub created_unix: u64,
    pub memory_bytes: u64,
    pub millicpus: u32,
    pub disk_bytes: u64,
    pub build_memory_bytes: u64,
    pub ports: Vec<u16>,
    /// The plan the person approved when it was created.
    pub plan_digest: String,
    /// The engine log level it was created at. A sandbox older than this field
    /// has none, and is treated as recording nothing rather than guessed at.
    #[serde(default)]
    pub log_level: Option<String>,
    /// Every host it can reach while running, as created. `None` for a
    /// sandbox made before this was recorded, which is not the same as none.
    #[serde(default)]
    pub reachable_hosts: Option<Vec<String>>,
    /// The commit the host made of the snapshot it was given, which changes
    /// inside it are compared with. `None` when there is none to compare with.
    #[serde(default)]
    pub baseline: Option<String>,
}

const RECORD_FILE: &str = "sandbox.json";

impl SandboxRecord {
    pub fn load(project_dir: &Path) -> Result<Option<Self>, String> {
        read_json(&project_dir.join(RECORD_FILE))
    }

    pub fn save(&self, project_dir: &Path) -> Result<(), String> {
        write_json(&project_dir.join(RECORD_FILE), self)
    }

    pub fn remove(project_dir: &Path) -> Result<(), String> {
        match std::fs::remove_file(project_dir.join(RECORD_FILE)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("cannot remove the sandbox record: {}", e.kind())),
        }
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    use std::io::Read;
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Err("a settings file is not a regular file".to_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot inspect a settings file: {}", e.kind())),
    }
    let file =
        std::fs::File::open(path).map_err(|e| format!("cannot open settings: {}", e.kind()))?;
    let mut text = String::new();
    file.take(MAX_FILE_BYTES)
        .read_to_string(&mut text)
        .map_err(|e| format!("cannot read settings: {}", e.kind()))?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|_| "a settings file is damaged or from another version".to_owned())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| "cannot encode settings".to_owned())?;
    hoststate::write_private(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_settings_give_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(UserSettings::load(dir.path()).unwrap(), UserSettings::default());
    }

    #[test]
    fn settings_survive_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let settings = UserSettings {
            max_memory_bytes: 3 * GIB,
            ..Default::default()
        };
        settings.save(dir.path()).unwrap();
        assert_eq!(UserSettings::load(dir.path()).unwrap(), settings);
    }

    /// Falling back to the defaults here would raise a ceiling the person
    /// lowered, so a damaged file has to stop the command.
    #[test]
    fn damaged_settings_do_not_fall_back_to_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SETTINGS_FILE), b"{ not json").unwrap();
        assert!(UserSettings::load(dir.path()).is_err());
    }

    #[test]
    fn an_unknown_settings_field_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            br#"{"version":1,"max_memory_bytes":1,"max_cpus_millis":1,"max_disk_bytes":1,
                 "max_build_memory_bytes":null,"memory_reserve_bytes":1,"disk_reserve_bytes":1,
                 "cpu_reserve_millis":1,"max_everything":true}"#,
        )
        .unwrap();
        assert!(UserSettings::load(dir.path()).is_err());
    }

    #[test]
    fn a_zero_ceiling_is_an_error_not_a_quiet_replacement() {
        let settings = UserSettings {
            max_memory_bytes: 0,
            ..Default::default()
        };
        assert!(settings.limits().is_err());
        assert!(UserSettings::default().limits().is_ok());
    }

    #[test]
    fn approvals_start_empty_and_deduplicate_on_save() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Approvals::load(dir.path()).unwrap(), Approvals::default());
        let approvals = Approvals {
            version: 0,
            ports: vec![3000, 8080, 3000],
            run_hosts: vec!["b.example.com".into(), "a.example.com".into()],
        };
        approvals.save(dir.path()).unwrap();
        let back = Approvals::load(dir.path()).unwrap();
        assert_eq!(back.ports, vec![3000, 8080]);
        assert_eq!(back.run_hosts, vec!["a.example.com", "b.example.com"]);
        assert!(back.allows_port(3000));
        assert!(!back.allows_port(5432));
    }

    #[test]
    fn a_sandbox_record_survives_a_round_trip_and_can_be_removed() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(SandboxRecord::load(dir.path()).unwrap(), None);
        let record = SandboxRecord {
            version: VERSION,
            created_unix: 42,
            memory_bytes: 4 * GIB,
            millicpus: 4000,
            disk_bytes: 30 * GIB,
            build_memory_bytes: 4 * GIB,
            ports: vec![3000],
            plan_digest: "abc".to_owned(),
            log_level: Some("debug".to_owned()),
            reachable_hosts: Some(vec!["registry.npmjs.org".to_owned()]),
            baseline: Some("a".repeat(40)),
        };
        record.save(dir.path()).unwrap();
        assert_eq!(SandboxRecord::load(dir.path()).unwrap(), Some(record));
        SandboxRecord::remove(dir.path()).unwrap();
        assert_eq!(SandboxRecord::load(dir.path()).unwrap(), None);
        SandboxRecord::remove(dir.path()).unwrap();
    }

    /// Sandboxes made before the level was recorded have a file like this one.
    #[test]
    fn a_record_from_before_the_level_was_kept_has_no_level() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(RECORD_FILE),
            br#"{"version":1,"created_unix":1,"memory_bytes":1,"millicpus":1,"disk_bytes":1,
                 "build_memory_bytes":1,"ports":[],"plan_digest":"abc"}"#,
        )
        .unwrap();
        let record = SandboxRecord::load(dir.path()).unwrap().unwrap();
        assert_eq!(record.log_level, None);
        assert_eq!(record.baseline, None);
    }

    #[test]
    fn settings_are_written_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        UserSettings::default().save(dir.path()).unwrap();
        let mode = std::fs::metadata(dir.path().join(SETTINGS_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "mode was {mode:o}");
    }
}
