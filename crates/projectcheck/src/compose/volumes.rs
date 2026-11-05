//! Service mounts, in short and long syntax.

use serde_norway::Value;

use super::interp::interpolate;
use super::service::{flag, Flag};
use super::{
    key_text, show, Ctx, FindingKind, Mount, MountKind, PathClass, Severity, UNKNOWN_VALUE,
};
use crate::clean;
use crate::safe_fs::is_env_file_name;

const SHORT_OPTIONS: [&str; 14] = [
    "ro",
    "rw",
    "z",
    "Z",
    "cached",
    "delegated",
    "consistent",
    "nocopy",
    "shared",
    "slave",
    "private",
    "rshared",
    "rslave",
    "rprivate",
];

pub(super) fn analyze(service: &str, value: &Value, ctx: &mut Ctx) -> Vec<Mount> {
    let items: Vec<&Value> = match value {
        Value::Null => return Vec::new(),
        Value::Sequence(items) => items.iter().collect(),
        Value::String(_) => {
            ctx.unrecognized(Some(service), "`volumes` is not a list");
            vec![value]
        }
        _ => {
            ctx.unrecognized(Some(service), "`volumes` has an unknown shape");
            return Vec::new();
        }
    };
    let mut mounts = Vec::new();
    for item in items {
        let mount = match item {
            Value::String(text) => short_form(service, text, ctx),
            Value::Mapping(_) => long_form(service, item, ctx),
            other => {
                ctx.unrecognized(
                    Some(service),
                    &format!(
                        "`volumes` has an entry of an unknown shape: {}",
                        show(other)
                    ),
                );
                None
            }
        };
        mounts.extend(mount);
    }
    mounts
}

/// Splits `source:target[:mode]`, keeping a Windows drive with its path.
fn split_short(text: &str) -> Vec<&str> {
    let parts: Vec<&str> = text.split(':').collect();
    let has_drive = parts.len() >= 3
        && parts[0].len() == 1
        && parts[0].as_bytes()[0].is_ascii_alphabetic()
        && (parts[1].starts_with('\\') || parts[1].starts_with('/'));
    if !has_drive {
        return parts;
    }
    let drive_end = parts[0].len() + 1 + parts[1].len();
    let mut joined = vec![&text[..drive_end]];
    joined.extend_from_slice(&parts[2..]);
    joined
}

fn short_form(service: &str, raw: &str, ctx: &mut Ctx) -> Option<Mount> {
    let text = interpolate(raw);
    let parts = split_short(&text.value);
    match parts.as_slice() {
        [target] => {
            let looks_like_target = target.starts_with('/') && !target.contains(UNKNOWN_VALUE);
            if !looks_like_target {
                ctx.unrecognized(
                    Some(service),
                    &format!("mount `{}` has an unrecognized form", clean(raw)),
                );
            }
            if is_engine_socket(target) {
                report_socket(service, target, ctx);
            }
            Some(Mount {
                kind: if looks_like_target {
                    MountKind::Anonymous
                } else {
                    MountKind::Host
                },
                source: None,
                target: Some(clean(target)),
                read_only: false,
                dynamic: text.dynamic,
            })
        }
        [source, target] => Some(classify_mount(
            service,
            source,
            target,
            false,
            text.dynamic,
            false,
            ctx,
        )),
        [source, target, options] => {
            let mut read_only = false;
            let mut writable = false;
            for option in options.split(',') {
                match option {
                    "ro" => read_only = true,
                    "rw" => writable = true,
                    _ => {}
                }
                if !SHORT_OPTIONS.contains(&option) {
                    ctx.unrecognized(
                        Some(service),
                        &format!("mount `{}` has an unrecognized option", clean(raw)),
                    );
                }
                if matches!(option, "shared" | "rshared" | "slave" | "rslave") {
                    report_propagation(service, raw, ctx);
                }
            }
            Some(classify_mount(
                service,
                source,
                target,
                read_only && !writable,
                text.dynamic,
                false,
                ctx,
            ))
        }
        _ => {
            ctx.unrecognized(
                Some(service),
                &format!("mount `{}` has an unrecognized form", clean(raw)),
            );
            None
        }
    }
}

fn long_form(service: &str, value: &Value, ctx: &mut Ctx) -> Option<Mount> {
    let map = value.as_mapping()?;
    let mut kind = None;
    let mut source = None;
    let mut target = None;
    let mut read_only = false;
    let mut dynamic = false;
    for (key, item) in map {
        let key = key_text(key);
        match key.as_str() {
            "type" => kind = item.as_str().map(str::to_string),
            "source" | "target" => match item.as_str().map(interpolate) {
                Some(text) => {
                    dynamic |= text.dynamic;
                    if key == "source" {
                        source = Some(text.value);
                    } else {
                        target = Some(text.value);
                    }
                }
                None => ctx.unrecognized(
                    Some(service),
                    &format!("long-form mount has `{key}` that is not text"),
                ),
            },
            "read_only" => match flag(item) {
                Flag::On => read_only = true,
                Flag::Off => {}
                // Read-only cannot be relied on, so it counts as writable.
                _ => ctx.unrecognized(
                    Some(service),
                    &format!(
                        "long-form mount has an unrecognized `read_only`: {}",
                        show(item)
                    ),
                ),
            },
            "bind" => check_bind_options(service, item, ctx),
            "volume" | "tmpfs" | "image" | "consistency" => {}
            other if other.starts_with("x-") => {}
            other => ctx.unrecognized(
                Some(service),
                &format!("long-form mount has unknown field `{other}`"),
            ),
        }
    }
    let target_text = target.as_deref().unwrap_or("");
    if target.is_none() {
        ctx.unrecognized(Some(service), "long-form mount has no `target`");
    }
    match kind.as_deref() {
        Some("tmpfs") => Some(Mount {
            kind: MountKind::Tmpfs,
            source: None,
            target: target.as_deref().map(clean),
            read_only,
            dynamic,
        }),
        Some("npipe") => {
            let pipe = source.as_deref().unwrap_or("");
            ctx.push(
                if pipe.to_ascii_lowercase().contains("docker_engine") {
                    Severity::Critical
                } else {
                    Severity::High
                },
                FindingKind::DockerSocket,
                Some(service),
                &format!("mounts the Windows named pipe `{pipe}`"),
            );
            Some(Mount {
                kind: MountKind::Host,
                source: Some(clean(pipe)),
                target: target.as_deref().map(clean),
                read_only,
                dynamic,
            })
        }
        Some("bind") => match source {
            Some(source) => Some(classify_mount(
                service,
                &source,
                target_text,
                read_only,
                dynamic,
                true,
                ctx,
            )),
            None => {
                ctx.unrecognized(Some(service), "long-form bind mount has no `source`");
                Some(Mount {
                    kind: MountKind::Host,
                    source: None,
                    target: target.as_deref().map(clean),
                    read_only,
                    dynamic,
                })
            }
        },
        Some("volume") => match source {
            Some(source) => Some(classify_mount(
                service,
                &source,
                target_text,
                read_only,
                dynamic,
                false,
                ctx,
            )),
            None => Some(Mount {
                kind: MountKind::Anonymous,
                source: None,
                target: target.as_deref().map(clean),
                read_only,
                dynamic,
            }),
        },
        other => {
            if other != Some("image") {
                ctx.unrecognized(
                    Some(service),
                    &format!(
                        "long-form mount has an unrecognized `type`: {}",
                        clean(other.unwrap_or("none"))
                    ),
                );
            }
            // A source is still checked, whatever the type claims to be.
            let is_image = other == Some("image");
            match source {
                Some(source) if !is_image => Some(classify_mount(
                    service,
                    &source,
                    target_text,
                    read_only,
                    dynamic,
                    true,
                    ctx,
                )),
                source => Some(Mount {
                    kind: MountKind::Other,
                    source: source.as_deref().map(clean),
                    target: target.as_deref().map(clean),
                    read_only,
                    dynamic,
                }),
            }
        }
    }
}

fn check_bind_options(service: &str, value: &Value, ctx: &mut Ctx) {
    let Some(map) = value.as_mapping() else {
        ctx.unrecognized(
            Some(service),
            "long-form mount has `bind` of an unknown shape",
        );
        return;
    };
    for (key, item) in map {
        let key = key_text(key);
        match key.as_str() {
            "propagation" => {
                if !matches!(item.as_str(), Some("private" | "rprivate")) {
                    report_propagation(service, &show(item), ctx);
                }
            }
            "create_host_path" | "selinux" | "recursive" => {}
            other if other.starts_with("x-") => {}
            other => ctx.unrecognized(
                Some(service),
                &format!("long-form mount has unknown bind option `{other}`"),
            ),
        }
    }
}

fn report_propagation(service: &str, what: &str, ctx: &mut Ctx) {
    ctx.push(
        Severity::Medium,
        FindingKind::HostMount,
        Some(service),
        &format!(
            "mount {} uses shared propagation, so mounts can pass between container and host",
            clean(what)
        ),
    );
}

/// Records one mount and reports it. `always_path` is set for a long-form bind,
/// where even a bare name is a path relative to the project.
fn classify_mount(
    service: &str,
    source: &str,
    target: &str,
    read_only: bool,
    dynamic: bool,
    always_path: bool,
    ctx: &mut Ctx,
) -> Mount {
    let access = if read_only { "read-only" } else { "writable" };
    let source_is_socket = is_engine_socket(source) || holds_engine_socket(source);
    if source_is_socket || is_engine_socket(target) {
        report_socket(service, if source_is_socket { source } else { target }, ctx);
        return Mount {
            kind: MountKind::Host,
            source: Some(clean(source)),
            target: Some(clean(target)),
            read_only,
            dynamic,
        };
    }
    let is_path = always_path
        || source.contains(UNKNOWN_VALUE)
        || source.contains('/')
        || source.contains('\\')
        || source.starts_with('.')
        || source.starts_with('~')
        || source.is_empty();
    let kind = if !is_path {
        if dynamic {
            ctx.push(
                Severity::Medium,
                FindingKind::HostMount,
                Some(service),
                &format!("mount source `{source}` comes from a variable, which can change it to a host path"),
            );
        }
        MountKind::Named
    } else {
        match ctx.classify(source) {
            PathClass::Inside(rel) => {
                ctx.check_symlink(Some(service), &rel);
                if is_env_file_name(std::path::Path::new(&rel)) {
                    ctx.push(
                        Severity::Medium,
                        FindingKind::EnvFileMount,
                        Some(service),
                        &format!("mounts environment file {rel}, which may hold secrets"),
                    );
                }
                ctx.push(
                    Severity::Info,
                    FindingKind::ProjectMount,
                    Some(service),
                    &format!("mounts project path {source} at {target} ({access})"),
                );
                if dynamic {
                    ctx.push(
                        Severity::Medium,
                        FindingKind::HostMount,
                        Some(service),
                        &format!("mount source {source} comes from a variable, which can change it to a host path"),
                    );
                }
                MountKind::Project
            }
            outside => {
                ctx.push(
                    Severity::High,
                    FindingKind::HostMount,
                    Some(service),
                    &format!(
                        "mounts {source} from outside the project at {target} ({access}, {})",
                        outside.describe()
                    ),
                );
                MountKind::Host
            }
        }
    };
    Mount {
        kind,
        source: Some(clean(source)),
        target: Some(clean(target)),
        read_only,
        dynamic,
    }
}

fn report_socket(service: &str, path: &str, ctx: &mut Ctx) {
    ctx.push(
        Severity::Critical,
        FindingKind::DockerSocket,
        Some(service),
        &format!("mounts the container engine socket, or a directory holding it: {path}"),
    );
}

/// Path parts in lower case, with `..`, repeated slashes and backslashes
/// resolved, so that none of them hide a match.
fn normalized(path: &str) -> String {
    let lower = path.to_ascii_lowercase().replace('\\', "/");
    let mut stack: Vec<&str> = Vec::new();
    for part in lower.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            name => stack.push(name),
        }
    }
    stack.join("/")
}

fn is_engine_socket(path: &str) -> bool {
    let path = normalized(path);
    ["docker", "podman", "containerd", "crio", "cri-dockerd"]
        .iter()
        .any(|engine| path.contains(&format!("{engine}.sock")))
}

/// True for a host directory that has the engine socket inside it. This
/// applies to sources only: a volume mounted at `/run` in a container is fine.
fn holds_engine_socket(source: &str) -> bool {
    source.starts_with('/') && matches!(normalized(source).as_str(), "" | "var" | "run" | "var/run")
}
