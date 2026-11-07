//! `resources`: what the repo asks for and what it would get.

use std::path::PathBuf;

use clap::Args;
use projectcheck::resources::{
    load, resolve, ByteSize, Clamped, HostResources, MilliCpus, ResourceRequest, UserLimits,
    DEFAULT_CPUS, DEFAULT_DISK, DEFAULT_MEMORY,
};

use crate::host::HostInfo;
use crate::ui::{cell, esc, esc_path, human_bytes, not_protecting, paint, Tone};
use crate::{exit, project_dir, say, tell, Deps};

const GIB: u64 = 1 << 30;
const MEMORY_RESERVE: u64 = 2 * GIB;
const DISK_RESERVE: u64 = 10 * GIB;
const CPU_RESERVE_MILLIS: u32 = 1000;

#[derive(Args)]
pub struct ResourcesArgs {
    /// Project folder [default: the current folder]
    path: Option<PathBuf>,
    /// Use this memory limit instead of the repo's, such as 6G
    #[arg(long, value_name = "SIZE")]
    memory: Option<ByteSize>,
    /// Use this CPU limit instead of the repo's, such as 4 or 1.5
    #[arg(long, value_name = "COUNT")]
    cpus: Option<MilliCpus>,
}

/// Computes the host-side numbers. There is no user setting yet, so the user's
/// maximum comes from the host, on whole GiB: at most three quarters of its
/// memory, and never more than is free now above the reserve. A greedy repo is
/// lowered to that, and a refusal means the host really has too little left.
fn host_limits(info: &HostInfo) -> Result<(UserLimits, HostResources), String> {
    let total = info.memory_total.ok_or("could not read total memory")?;
    let available = info.memory_available.ok_or("could not read free memory")?;
    let cpus = info.cpus.ok_or("could not read the CPU count")?;
    let disk = info.disk_free.ok_or("could not read free disk space")?;

    let whole_gib = |bytes: u64| ByteSize::new((bytes / GIB).max(1) * GIB);
    let millis = cpus.saturating_mul(1000);
    let limits = UserLimits {
        max_memory: whole_gib((total / 4 * 3).min(available.saturating_sub(MEMORY_RESERVE)))
            .ok_or("could not compute a memory limit")?,
        max_cpus: MilliCpus::new(millis.saturating_sub(CPU_RESERVE_MILLIS).max(1000))
            .ok_or("could not compute a CPU limit")?,
        max_disk: whole_gib(disk.saturating_sub(DISK_RESERVE))
            .ok_or("could not compute a disk limit")?,
        max_build_memory: None,
    };
    let host = HostResources {
        available_memory: available,
        memory_reserve: MEMORY_RESERVE,
        available_millicpus: millis,
        cpu_reserve_millis: CPU_RESERVE_MILLIS,
        available_disk: disk,
        disk_reserve: DISK_RESERVE,
    };
    Ok((limits, host))
}

/// The line `doctor` prints: the defaults, fitted to this host.
pub fn suggestion(info: &HostInfo) -> Option<String> {
    let (limits, _) = host_limits(info).ok()?;
    Some(format!(
        "{} memory, {}, {} disk",
        DEFAULT_MEMORY.min(limits.max_memory),
        proto::cpus_text(DEFAULT_CPUS.min(limits.max_cpus).millis()),
        DEFAULT_DISK.min(limits.max_disk),
    ))
}

fn opt<T: ToString>(value: Option<T>) -> String {
    value.map_or_else(|| "-".to_string(), |v| v.to_string())
}

pub fn run(args: &ResourcesArgs, deps: &mut Deps) -> i32 {
    let root = match project_dir(args.path.as_deref(), deps) {
        Ok(root) => root,
        Err(reason) => {
            tell!(deps, "resources: {reason}");
            return exit::FAILED;
        }
    };
    let requested = match load(&root) {
        Ok(request) => request,
        Err(error) => {
            tell!(
                deps,
                "resources: {} could not be used: {}",
                brand::CONFIG_FILE,
                esc(&error.to_string())
            );
            return exit::FAILED;
        }
    };
    let info = (deps.host)(&root);
    let (limits, host) = match host_limits(&info) {
        Ok(found) => found,
        Err(reason) => {
            tell!(deps, "resources: {reason}");
            return exit::FAILED;
        }
    };
    let effective = ResourceRequest {
        memory: args.memory.or(requested.memory),
        cpus: args.cpus.or(requested.cpus),
        ..requested
    };

    say!(
        deps,
        "{}{}",
        cell(deps.color, Tone::Strong, "Project", 13),
        esc_path(&root)
    );
    match resolve(&effective, &limits, &host) {
        Ok(resolved) => {
            say!(deps);
            let header = format!(
                "{:<13}{:<11}{:<11}{:<11}{}",
                "", "Repo asks", "Override", "Maximum", "Resolved"
            );
            say!(deps, "{}", paint(deps.color, Tone::Strong, &header));
            let line = |label: &str, repo: String, over: String, max: String, value: String| {
                format!("{label:<13}{repo:<11}{over:<11}{max:<11}{value}")
            };
            say!(
                deps,
                "{}",
                line(
                    "Memory",
                    opt(requested.memory),
                    opt(args.memory),
                    limits.max_memory.to_string(),
                    resolved.memory.to_string()
                )
            );
            say!(
                deps,
                "{}",
                line(
                    "CPUs",
                    opt(requested.cpus),
                    opt(args.cpus),
                    limits.max_cpus.to_string(),
                    resolved.cpus.to_string()
                )
            );
            say!(
                deps,
                "{}",
                line(
                    "Disk",
                    opt(requested.disk),
                    "-".to_string(),
                    limits.max_disk.to_string(),
                    resolved.disk.to_string()
                )
            );
            say!(
                deps,
                "{}",
                line(
                    "Build memory",
                    opt(requested.build_memory),
                    "-".to_string(),
                    limits.max_memory.to_string(),
                    resolved.build_memory.to_string()
                )
            );
            say!(deps);
            for item in &resolved.clamped {
                let name = match item {
                    Clamped::Memory => "memory",
                    Clamped::Cpus => "CPUs",
                    Clamped::Disk => "disk",
                    Clamped::BuildMemory => "build memory",
                };
                let text = format!("Lowered {name} to your maximum.");
                say!(deps, "{}", paint(deps.color, Tone::Warn, &text));
            }
            host_lines(&host, deps);
            say!(deps);
            say!(
                deps,
                "No VM was started, so these limits are not applied to anything."
            );
            say!(deps, "{}", paint(deps.color, Tone::Warn, &not_protecting()));
            exit::OK
        }
        Err(error) => {
            host_lines(&host, deps);
            say!(deps);
            let word = paint(deps.color, Tone::Bad, "Refused");
            tell!(deps, "{word}: {}", esc(&error.to_string()));
            tell!(
                deps,
                "Next: lower the limit with --memory or --cpus, or free up the host."
            );
            exit::FAILED
        }
    }
}

fn host_lines(host: &HostResources, deps: &mut Deps) {
    say!(
        deps,
        "Host: about {} memory free, keeping {} back; {} disk free, keeping {} back.",
        human_bytes(host.available_memory),
        human_bytes(host.memory_reserve),
        human_bytes(host.available_disk),
        human_bytes(host.disk_reserve),
    );
    say!(
        deps,
        "The maximum comes from this host's current state. There is no user setting yet."
    );
}
