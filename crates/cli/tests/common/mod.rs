//! Shared helpers. The real keychain is out of bounds for every test here:
//! [`run_binary`] refuses any arguments that could reach the secret store, and
//! commands that use the store run in-process with a memory store instead.

#![allow(dead_code)]

use std::ffi::OsString;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use cli::{exit, DaemonClient, Deps, HostInfo, Hypervisor, NoDaemon};
use secrets::{MemoryStore, ProjectId, SecretName, SecretStore, SecretValue, StoreError};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

pub const GIB: u64 = 1 << 30;

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

pub fn home_var() -> String {
    format!("{}{}", brand::ENV_PREFIX, brand::HOME_ENV_SUFFIX)
}

/// The binary sits next to the `deps` folder holding this test executable.
/// The usual compile-time variable would write the product name in this file.
pub fn binary() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join(brand::CLI_BIN)
}

pub struct Output {
    pub code: i32,
    pub out: String,
    pub err: String,
}

impl Output {
    pub fn all(&self) -> String {
        format!("{}{}", self.out, self.err)
    }
}

/// True if these arguments could make the real binary use the secret store.
fn reaches_store(args: &[&str]) -> bool {
    if args.first() != Some(&"secrets") {
        return false;
    }
    match args.get(1).copied() {
        Some("list") | Some("--help") | None => false,
        Some("import") => !args.contains(&"--dry-run"),
        _ => !args.contains(&"--help"),
    }
}

/// Runs the real binary with its home pointed at `home`.
pub fn run_binary(home: &Path, args: &[&str]) -> Output {
    run_binary_with(home, args, &[], "")
}

pub fn run_binary_with(home: &Path, args: &[&str], env: &[(&str, &str)], stdin: &str) -> Output {
    assert!(
        !reaches_store(args),
        "this test would touch the real keychain: {args:?}"
    );
    spawn_binary(home, args, env, stdin)
}

/// Runs arguments that the parser must refuse before any store exists. They
/// run in-process against a memory store first, and the real binary is started
/// only once that has shown they stop early.
pub fn run_binary_expecting_refusal(home: &Path, args: &[&str]) -> Output {
    let harness = Harness::new();
    let early = harness.run(args, "");
    assert_eq!(early.code, exit::USAGE, "{args:?}: {}", early.all());
    assert!(!harness.state().exists(), "{args:?} got past the refusal");
    spawn_binary(home, args, &[], "")
}

fn spawn_binary(home: &Path, args: &[&str], env: &[(&str, &str)], stdin: &str) -> Output {
    use std::io::Write;
    let mut child = Command::new(binary())
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env(home_var(), home)
        .envs(env.iter().copied())
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let done = child.wait_with_output().unwrap();
    Output {
        code: done.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&done.stdout).into_owned(),
        err: String::from_utf8_lossy(&done.stderr).into_owned(),
    }
}

/// A memory store per project that outlives each command, like a keychain.
#[derive(Clone, Default)]
pub struct Vault {
    projects: Arc<Mutex<HashMap<ProjectId, Arc<MemoryStore>>>>,
}

struct VaultStore(Arc<MemoryStore>);

impl SecretStore for VaultStore {
    fn put(&self, name: &SecretName, value: &SecretValue) -> Result<(), StoreError> {
        self.0.put(name, value)
    }
    fn get(&self, name: &SecretName) -> Result<Option<SecretValue>, StoreError> {
        self.0.get(name)
    }
    fn delete(&self, name: &SecretName) -> Result<bool, StoreError> {
        self.0.delete(name)
    }
    fn names(&self) -> Result<Option<Vec<SecretName>>, StoreError> {
        self.0.names()
    }
}

impl Vault {
    fn open(&self, project: &ProjectId) -> Box<dyn SecretStore> {
        let mut projects = self.projects.lock().unwrap();
        let store = projects.entry(project.clone()).or_default().clone();
        Box::new(VaultStore(store))
    }

    /// Every `<project id>/<name>` held, as the keychain would list them.
    pub fn accounts(&self) -> Vec<String> {
        let projects = self.projects.lock().unwrap();
        let mut all = Vec::new();
        for (project, store) in projects.iter() {
            for name in store.names().unwrap().unwrap() {
                all.push(format!("{project}/{name}"));
            }
        }
        all.sort();
        all
    }

    /// Puts a value straight into the store, as an older build might have.
    pub fn put(&self, project: &str, name: &str, value: &str) {
        self.open(&ProjectId::new(project).unwrap())
            .put(&SecretName::new(name).unwrap(), &SecretValue::new(value))
            .unwrap();
    }

    /// Reads a value from whichever project holds it.
    pub fn value(&self, name: &str) -> Option<String> {
        let name = SecretName::new(name).unwrap();
        let projects = self.projects.lock().unwrap();
        projects.values().find_map(|store| {
            store
                .get(&name)
                .unwrap()
                .map(|value| value.expose().to_string())
        })
    }
}

pub fn big_host() -> HostInfo {
    HostInfo {
        os: "macOS 26.0".to_string(),
        arch: "aarch64".to_string(),
        hypervisor: Hypervisor::Available("Hypervisor.framework is supported".to_string()),
        cpus: Some(8),
        memory_total: Some(32 * GIB),
        memory_available: Some(20 * GIB),
        disk_free: Some(200 * GIB),
    }
}

/// One in-process environment: a temp home, a memory store, a fake host.
pub struct Harness {
    pub home: TempDir,
    pub project: TempDir,
    pub vault: Vault,
    pub host: HostInfo,
    pub color: bool,
    /// What commands are told about stdin and stdout being a terminal.
    pub terminal: bool,
    /// Answers for the hidden prompt, in order.
    pub answers: Mutex<Vec<String>>,
    /// Where requests go. Without one, every command that needs the daemon
    /// fails the same way, which is what the real binary does with no daemon
    /// running.
    pub daemon: Option<Box<dyn DaemonClient + Send + Sync>>,
}

/// Stands in for a daemon that is not there.
pub static NO_DAEMON: NoDaemon = NoDaemon {
    reason: String::new(),
};

impl Harness {
    pub fn new() -> Self {
        Self {
            // Short on purpose: the default macOS temp path fails the socket check.
            home: tempfile::Builder::new()
                .prefix("h")
                .tempdir_in("/tmp")
                .unwrap(),
            project: TempDir::new().unwrap(),
            vault: Vault::default(),
            host: big_host(),
            color: false,
            terminal: false,
            answers: Mutex::new(Vec::new()),
            daemon: None,
        }
    }

    /// Points this harness at something that answers requests.
    #[must_use]
    pub fn with_daemon(mut self, daemon: Box<dyn DaemonClient + Send + Sync>) -> Self {
        self.daemon = Some(daemon);
        self
    }

    pub fn answer(&self, values: &[&str]) {
        let mut answers = self.answers.lock().unwrap();
        *answers = values.iter().rev().map(|v| v.to_string()).collect();
    }

    pub fn state(&self) -> PathBuf {
        self.home.path().join(brand::STATE_DIR)
    }

    pub fn run(&self, args: &[&str], stdin: &str) -> Output {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut input = Cursor::new(stdin.as_bytes().to_vec());
        let home: OsString = self.home.path().into();
        let var = home_var();
        let env = |key: &str| (key == var).then(|| home.clone());
        let vault = self.vault.clone();
        let store = move |project: &ProjectId| vault.open(project);
        let listed = self.vault.clone();
        let accounts = move || Ok(Some(listed.accounts()));
        let prompt = |_: &str| {
            let next = self.answers.lock().unwrap().pop();
            next.map(SecretValue::new)
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotConnected))
        };
        let host = |_: &Path| self.host.clone();
        let full: Vec<&str> = std::iter::once(brand::CLI_BIN)
            .chain(args.iter().copied())
            .collect();
        let code = cli::run(
            full,
            &mut Deps {
                out: &mut out,
                err: &mut err,
                input: &mut input,
                color: self.color,
                terminal: self.terminal,
                cwd: self.project.path().to_path_buf(),
                env: &env,
                store: &store,
                accounts: &accounts,
                prompt: &prompt,
                host: &host,
                daemon: self.daemon.as_ref().map_or(&NO_DAEMON as &dyn DaemonClient, |d| d.as_ref()),
            },
        );
        Output {
            code,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }
}

pub const NOT_BUILT: i32 = exit::NOT_BUILT;

/// Reads every file under a folder, for checking that no value was written.
pub fn read_all(dir: &Path) -> String {
    let mut text = String::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                text.push_str(&String::from_utf8_lossy(&std::fs::read(path).unwrap()));
            }
        }
    }
    text
}
