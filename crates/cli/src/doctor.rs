//! `doctor`: host checks. Nothing here needs Docker, and nothing is written.

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::Duration;

use engine::{EngineError, MsbEngine, MsbLocation, DERIVED_SOCKET_SUFFIX_BYTES, TESTED_VERSION};

use crate::host::{HostInfo, Hypervisor};
use crate::state::{engine_bin_var, inspect_state, StateCheck, StatePaths, ENGINE_DIR};
use crate::ui::{cell, esc, esc_path, human_bytes, not_protecting, paint, Tone};
use crate::{exit, resources, say, Deps};

const GIB: u64 = 1 << 30;
const SOCKET_LIMIT_MACOS: usize = 104;
const SOCKET_LIMIT_LINUX: usize = 108;
const ENGINE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Ok,
    Warn,
    Fail,
}

struct Row {
    label: &'static str,
    level: Level,
    detail: String,
    next: Option<String>,
}

fn row(label: &'static str, level: Level, detail: impl Into<String>) -> Row {
    Row {
        label,
        level,
        detail: detail.into(),
        next: None,
    }
}

impl Row {
    fn next(mut self, next: impl Into<String>) -> Self {
        self.next = Some(next.into());
        self
    }
}

pub fn run(deps: &mut Deps) -> i32 {
    let located = StatePaths::locate(deps.env);
    let disk_path = located
        .as_ref()
        .map_or_else(|_| deps.cwd.clone(), |p| p.state.clone());
    let info = (deps.host)(&disk_path);

    let mut rows = vec![platform(&info), hypervisor(&info), cpus(&info)];
    rows.push(memory(&info));
    rows.push(disk(&info));
    match &located {
        Ok(paths) => {
            rows.push(state_folder(paths));
            rows.push(socket(paths));
            rows.push(engine_row(paths, deps));
        }
        Err(reason) => rows.push(
            row("State folder", Level::Fail, esc(reason))
                .next("set HOME to an absolute path, or unset the override"),
        ),
    }

    for item in &rows {
        let (word, tone) = match item.level {
            Level::Ok => ("ok", Tone::Good),
            Level::Warn => ("warn", Tone::Warn),
            Level::Fail => ("fail", Tone::Bad),
        };
        say!(
            deps,
            "{}{}{}",
            cell(deps.color, Tone::Strong, item.label, 14),
            cell(deps.color, tone, word, 6),
            item.detail
        );
        if let Some(next) = &item.next {
            say!(deps, "{}{}{}", " ".repeat(14), "next  ", next);
        }
    }
    if let Some(line) = resources::suggestion(&info) {
        say!(
            deps,
            "{}      {line}",
            cell(deps.color, Tone::Strong, "Suggested", 14)
        );
    }

    let fails = rows.iter().filter(|r| r.level == Level::Fail).count();
    let warns = rows.iter().filter(|r| r.level == Level::Warn).count();
    say!(deps);
    say!(deps, "Failed: {fails}. Warnings: {warns}.");
    say!(deps, "{}", paint(deps.color, Tone::Warn, &not_protecting()));
    if fails > 0 {
        exit::FAILED
    } else {
        exit::OK
    }
}

fn platform(info: &HostInfo) -> Row {
    let detail = format!("{} on {}", esc(&info.os), esc(&info.arch));
    let is_mac = info.os.starts_with("macOS");
    if is_mac && info.arch == "aarch64" {
        row("Platform", Level::Ok, detail)
    } else if is_mac {
        row("Platform", Level::Fail, detail).next("the engine needs Apple Silicon on macOS")
    } else if info.os == "linux" {
        row("Platform", Level::Ok, detail)
    } else {
        row("Platform", Level::Fail, detail).next("use macOS on Apple Silicon, or Linux with KVM")
    }
}

fn hypervisor(info: &HostInfo) -> Row {
    match &info.hypervisor {
        Hypervisor::Available(detail) => row("Hypervisor", Level::Ok, esc(detail)),
        Hypervisor::Missing { detail, next } => {
            row("Hypervisor", Level::Fail, esc(detail)).next(esc(next))
        }
        Hypervisor::Unknown(detail) => row("Hypervisor", Level::Warn, esc(detail))
            .next("check your OS virtualization settings by hand"),
    }
}

fn cpus(info: &HostInfo) -> Row {
    match info.cpus {
        Some(count) if count >= 4 => row("CPU", Level::Ok, format!("{count} logical CPUs")),
        Some(1) => row("CPU", Level::Warn, "1 logical CPU")
            .next("builds will be slow; 4 or more is better"),
        Some(count) => row("CPU", Level::Warn, format!("{count} logical CPUs"))
            .next("builds will be slow; 4 or more is better"),
        None => row("CPU", Level::Warn, "could not read the CPU count")
            .next("report this as a bug, with your OS version"),
    }
}

fn memory(info: &HostInfo) -> Row {
    let Some(total) = info.memory_total else {
        return row("Memory", Level::Warn, "could not read total memory")
            .next("report this as a bug, with your OS version");
    };
    let free = info
        .memory_available
        .map_or("free unknown".to_string(), |b| {
            format!("about {} free", human_bytes(b))
        });
    let detail = format!("{} total, {free}", human_bytes(total));
    if total >= 16 * GIB {
        row("Memory", Level::Ok, detail)
    } else if total >= 8 * GIB {
        row("Memory", Level::Warn, detail).next("16 GB is recommended (an estimate, not measured)")
    } else {
        row("Memory", Level::Warn, detail)
            .next("below the estimated 8 GB minimum; use small limits with --memory")
    }
}

fn disk(info: &HostInfo) -> Row {
    match info.disk_free {
        Some(free) if free >= 30 * GIB => {
            row("Disk", Level::Ok, format!("{} free", human_bytes(free)))
        }
        Some(free) if free >= 10 * GIB => {
            row("Disk", Level::Warn, format!("{} free", human_bytes(free)))
                .next("a typical project needs about 30 GB; free up some space")
        }
        Some(free) => row("Disk", Level::Fail, format!("{} free", human_bytes(free)))
            .next("free at least 10 GB before using a sandbox"),
        None => row("Disk", Level::Warn, "could not read free disk space")
            .next("check it by hand with df"),
    }
}

fn state_folder(paths: &StatePaths) -> Row {
    let shown = esc_path(&paths.state);
    let note = if paths.overridden {
        " (set by override)"
    } else {
        ""
    };
    match inspect_state(&paths.state) {
        Ok(StateCheck::Missing) => row(
            "State folder",
            Level::Ok,
            format!("{shown}{note}, not created yet"),
        ),
        Ok(StateCheck::Foreign(reason)) => row(
            "State folder",
            Level::Fail,
            format!("{shown}{note}: {}", esc(&reason)),
        )
        .next("move that path aside, this tool will not use or remove it"),
        Ok(StateCheck::Ours(meta)) if meta.mode() & 0o077 != 0 => row(
            "State folder",
            Level::Warn,
            format!("{shown}{note}, others can read it"),
        )
        .next(format!("chmod 700 {shown}")),
        Ok(StateCheck::Ours(_)) => row(
            "State folder",
            Level::Ok,
            format!("{shown}{note}, owner-only"),
        ),
        Err(reason) => row(
            "State folder",
            Level::Fail,
            format!("{shown}{note}: {}", esc(&reason)),
        )
        .next("check the permissions of the parent folder"),
    }
}

fn socket_limit() -> usize {
    if cfg!(target_os = "macos") {
        SOCKET_LIMIT_MACOS
    } else {
        SOCKET_LIMIT_LINUX
    }
}

/// A Unix socket path must be shorter than the platform's `sun_path` limit.
/// The engine's own sockets sit deeper than the daemon's, so both are checked.
fn socket(paths: &StatePaths) -> Row {
    let limit = socket_limit();
    let daemon = paths.state.join(brand::SOCKET_FILE).as_os_str().len();
    let engine = engine_home(paths).as_os_str().len() + DERIVED_SOCKET_SUFFIX_BYTES;
    let detail = format!("daemon {daemon} bytes, engine {engine} bytes, must be under {limit}");
    if daemon < limit && engine < limit {
        row("Socket path", Level::Ok, detail)
    } else {
        row("Socket path", Level::Fail, detail).next(format!(
            "use a shorter home folder, or set {} to a short path",
            crate::state::home_override_var()
        ))
    }
}

fn engine_home(paths: &StatePaths) -> PathBuf {
    paths.state.join(ENGINE_DIR)
}

/// Never looks in the current folder or on `PATH`: a hostile repo could plant
/// a binary there, and this command is exactly what someone runs inside a repo
/// they do not trust yet. Only an explicit setting, the state folder or
/// Homebrew's fixed install path counts, the same as the daemon.
fn engine_location(paths: &StatePaths, deps: &Deps) -> (PathBuf, PathBuf) {
    let home = engine_home(paths);
    let binary = match (deps.env)(&engine_bin_var()) {
        Some(binary) => PathBuf::from(binary),
        None => hoststate::default_engine_bin(&paths.state),
    };
    (binary, home)
}

fn engine_row(paths: &StatePaths, deps: &Deps) -> Row {
    let (binary, home) = engine_location(paths, deps);
    let shown = esc_path(&binary);
    let found = MsbEngine::new(MsbLocation::Explicit {
        binary: binary.clone(),
        home,
    });
    let engine = match found {
        Ok(engine) => engine.with_timeout(ENGINE_TIMEOUT),
        Err(EngineError::BinaryNotFound { .. }) if binary.is_absolute() && !binary.exists() => {
            return row("Engine", Level::Warn, format!("not found at {shown}")).next(format!(
                "nothing needs it yet; to test it, set {} to the absolute path of msb",
                engine_bin_var()
            ))
        }
        Err(error) => {
            return row("Engine", Level::Fail, esc(&error.to_string())).next(format!(
                "{} must be an absolute path to the engine binary",
                engine_bin_var()
            ))
        }
    };
    let version = match engine.version() {
        Ok(version) => esc(&version),
        Err(error) => {
            return row("Engine", Level::Fail, esc(&error.to_string()))
                .next(format!("check that {shown} runs on this machine"))
        }
    };
    if let Err(error) = engine.preflight() {
        return row(
            "Engine",
            Level::Fail,
            format!("msb {version} at {shown}: {}", esc(&error.to_string())),
        )
        .next(format!(
            "use a shorter home folder, or set {} to a short path",
            crate::state::home_override_var()
        ));
    }
    if version == TESTED_VERSION {
        row("Engine", Level::Ok, format!("msb {version} at {shown}"))
    } else {
        row("Engine", Level::Warn, format!("msb {version} at {shown}"))
            .next(format!("the adapter was checked against {TESTED_VERSION}"))
    }
}
