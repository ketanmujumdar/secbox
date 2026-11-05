//! Checks for one service's fields.

use serde_norway::Value;

use super::interp::interpolate;
use super::{
    is_remote, key_text, ports, scalar_text, show, text_value, volumes, Ctx, FindingKind,
    PathClass, Replicas, Service, Severity,
};
use crate::clean;

/// More replicas than this in a default expression is worth a warning.
const LARGE_REPLICA_COUNT: u32 = 64;

/// Fields with no bearing on isolation, or with limits enforced by the VM.
const NEUTRAL_FIELDS: &[&str] = &[
    "annotations",
    "attach",
    "blkio_config",
    "cap_drop",
    "command",
    "container_name",
    "cpu_count",
    "cpu_percent",
    "cpu_period",
    "cpu_quota",
    "cpu_rt_period",
    "cpu_rt_runtime",
    "cpu_shares",
    "cpus",
    "cpuset",
    "depends_on",
    "dns",
    "dns_opt",
    "dns_search",
    "domainname",
    "entrypoint",
    "expose",
    "group_add",
    "healthcheck",
    "hostname",
    "init",
    "isolation",
    "labels",
    "links",
    "logging",
    "mac_address",
    "mem_limit",
    "mem_reservation",
    "mem_swappiness",
    "memswap_limit",
    "models",
    "networks",
    "oom_kill_disable",
    "oom_score_adj",
    "pids_limit",
    "platform",
    "pull_policy",
    "pull_refresh_after",
    "read_only",
    "restart",
    "shm_size",
    "stdin_open",
    "stop_grace_period",
    "stop_signal",
    "storage_opt",
    "tmpfs",
    "tty",
    "ulimits",
    "user",
    "working_dir",
];

const BUILD_FIELDS: &[&str] = &[
    "args",
    "cache_from",
    "cache_to",
    "dockerfile_inline",
    "extra_hosts",
    "isolation",
    "labels",
    "no_cache",
    "platforms",
    "provenance",
    "pull",
    "sbom",
    "secrets",
    "shm_size",
    "tags",
    "target",
    "ulimits",
];

const DEPLOY_FIELDS: &[&str] = &[
    "endpoint_mode",
    "labels",
    "mode",
    "placement",
    "resources",
    "restart_policy",
    "rollback_config",
    "update_config",
];

/// The state of a boolean field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Flag {
    Off,
    On,
    /// A variable sets it, and this is the value of its default.
    Variable(bool),
    /// A variable with no default sets it.
    Unresolved,
    /// Not a boolean in any form Compose accepts.
    Malformed,
}

pub(super) fn flag(value: &Value) -> Flag {
    match value {
        Value::Null => Flag::Off,
        Value::Bool(true) => Flag::On,
        Value::Bool(false) => Flag::Off,
        Value::String(text) => {
            let text = interpolate(text);
            if text.unresolved {
                return Flag::Unresolved;
            }
            let parsed = match text.value.to_ascii_lowercase().as_str() {
                "true" | "yes" | "y" | "on" | "1" => Some(true),
                "false" | "no" | "n" | "off" | "0" | "" => Some(false),
                _ => None,
            };
            match (parsed, text.dynamic) {
                (None, _) => Flag::Malformed,
                (Some(state), true) => Flag::Variable(state),
                (Some(true), false) => Flag::On,
                (Some(false), false) => Flag::Off,
            }
        }
        _ => Flag::Malformed,
    }
}

pub(super) fn analyze(raw_name: &str, value: &Value, ctx: &mut Ctx) -> Service {
    let name = clean(raw_name);
    let mut service = Service {
        name: name.clone(),
        image: None,
        has_build: false,
        replicas: Replicas {
            count: Some(1),
            expression: None,
            dynamic: false,
        },
        profiles: Vec::new(),
        ports: Vec::new(),
        mounts: Vec::new(),
        env_keys: Vec::new(),
    };
    let valid_name = !raw_name.is_empty()
        && raw_name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !valid_name {
        ctx.unrecognized(
            Some(&name),
            "the service name has characters Compose does not allow",
        );
    }
    let Some(map) = value.as_mapping() else {
        ctx.unrecognized(Some(&name), "the service is not a mapping");
        return service;
    };
    let mut saw_deploy_replicas = false;
    for (key, item) in map {
        let key = key_text(key);
        let svc = name.as_str();
        match key.as_str() {
            k if k.starts_with("x-") => {}
            k if NEUTRAL_FIELDS.contains(&k) => {}
            "image" => service.image = scalar_text(item).map(|text| clean(&text)),
            "privileged" => check_flag(
                svc,
                "privileged",
                item,
                FindingKind::Privileged,
                Severity::Critical,
                ctx,
            ),
            "use_api_socket" => check_flag(
                svc,
                "use_api_socket",
                item,
                FindingKind::DockerSocket,
                Severity::Critical,
                ctx,
            ),
            "network_mode" => check_mode(
                svc,
                "network_mode",
                item,
                FindingKind::NetworkMode,
                &["bridge", "none", "default"],
                true,
                ctx,
            ),
            "pid" => check_mode(
                svc,
                "pid",
                item,
                FindingKind::PidMode,
                &["", "private"],
                true,
                ctx,
            ),
            "ipc" => check_mode(
                svc,
                "ipc",
                item,
                FindingKind::IpcMode,
                &["", "none", "private", "shareable"],
                true,
                ctx,
            ),
            "uts" => check_mode(svc, "uts", item, FindingKind::UtsMode, &[""], false, ctx),
            "userns_mode" => check_mode(
                svc,
                "userns_mode",
                item,
                FindingKind::UsernsMode,
                &["", "private"],
                false,
                ctx,
            ),
            "cgroup" => check_mode(
                svc,
                "cgroup",
                item,
                FindingKind::CgroupMode,
                &["", "private"],
                false,
                ctx,
            ),
            "cap_add" => check_caps(svc, item, ctx),
            "security_opt" => check_security_opt(svc, item, ctx),
            "devices" => {
                for device in string_items(svc, "devices", item, ctx) {
                    ctx.push(
                        Severity::High,
                        FindingKind::Device,
                        Some(svc),
                        &format!(
                            "gives the service the device `{}`",
                            interpolate(&device).value
                        ),
                    );
                }
            }
            "device_cgroup_rules" => {
                for rule in string_items(svc, "device_cgroup_rules", item, ctx) {
                    ctx.push(
                        Severity::High,
                        FindingKind::Device,
                        Some(svc),
                        &format!("adds the device rule `{rule}`"),
                    );
                }
            }
            "volumes" => service.mounts = volumes::analyze(svc, item, ctx),
            "ports" => service.ports = ports::analyze(svc, item, ctx),
            "environment" => service.env_keys = env_keys(svc, item, ctx),
            "env_file" => check_env_file(Some(svc), item, ctx),
            "label_file" => {
                for file in string_items(svc, "label_file", item, ctx) {
                    check_read_file(svc, "label_file", &file, ctx);
                }
            }
            "build" => {
                service.has_build = true;
                check_build(svc, item, ctx);
            }
            "extends" => check_extends(svc, item, ctx),
            "profiles" => {
                service.profiles = string_items(svc, "profiles", item, ctx)
                    .iter()
                    .map(|p| clean(p))
                    .collect();
                if !service.profiles.is_empty() {
                    ctx.push(
                        Severity::Info,
                        FindingKind::Profile,
                        Some(svc),
                        &format!("starts only with profile: {}", service.profiles.join(", ")),
                    );
                }
            }
            "deploy" => {
                saw_deploy_replicas |= check_deploy(svc, item, &mut service.replicas, ctx);
            }
            "scale" => {
                if !saw_deploy_replicas {
                    service.replicas = parse_replicas(svc, item, ctx);
                }
            }
            "secrets" | "configs" => check_service_refs(svc, &key, item, ctx),
            "post_start" | "pre_start" | "pre_stop" => check_hooks(svc, &key, item, ctx),
            "extra_hosts" => check_extra_hosts(svc, item, ctx),
            "cgroup_parent" | "sysctls" | "gpus" | "credential_spec" | "develop"
            | "volumes_from" | "external_links" => {
                if !is_empty(item) {
                    ctx.push(
                        Severity::Medium,
                        FindingKind::Weakening,
                        Some(svc),
                        &format!("sets `{key}`, which can widen what the service reaches"),
                    );
                }
            }
            "runtime" | "provider" => {
                if !is_empty(item) {
                    ctx.push(
                        Severity::High,
                        FindingKind::Weakening,
                        Some(svc),
                        &format!(
                            "sets `{key}`, which changes what runs the service: {}",
                            show(item)
                        ),
                    );
                }
            }
            other => ctx.unrecognized(Some(svc), &format!("unknown service field `{other}`")),
        }
    }
    service
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Sequence(items) => items.is_empty(),
        Value::Mapping(map) => map.is_empty(),
        Value::String(text) => text.is_empty(),
        _ => false,
    }
}

fn check_flag(
    service: &str,
    field: &str,
    value: &Value,
    kind: FindingKind,
    severity: Severity,
    ctx: &mut Ctx,
) {
    match flag(value) {
        Flag::Off => {}
        Flag::On => ctx.push(severity, kind, Some(service), &format!("sets `{field}`")),
        Flag::Variable(true) => ctx.push(
            severity,
            kind,
            Some(service),
            &format!("sets `{field}` by default, through a variable"),
        ),
        Flag::Variable(false) => ctx.push(
            Severity::Medium,
            kind,
            Some(service),
            &format!("`{field}` is off by default, but a variable can turn it on"),
        ),
        Flag::Unresolved => ctx.push(
            severity,
            kind,
            Some(service),
            &format!("`{field}` depends on a variable with no default, so treat it as on"),
        ),
        Flag::Malformed => ctx.unrecognized(
            Some(service),
            &format!("`{field}` has an unrecognized value: {}", show(value)),
        ),
    }
}

/// Checks a namespace mode such as `network_mode`. `host` is reported, and so
/// is sharing with another service or container when `allow_refs` is set.
fn check_mode(
    service: &str,
    field: &str,
    value: &Value,
    kind: FindingKind,
    benign: &[&str],
    allow_refs: bool,
    ctx: &mut Ctx,
) {
    if value.is_null() {
        return;
    }
    let Some(text) = value.as_str().map(interpolate) else {
        ctx.unrecognized(
            Some(service),
            &format!("`{field}` has an unrecognized value: {}", show(value)),
        );
        return;
    };
    if text.unresolved {
        ctx.push(
            Severity::High,
            kind,
            Some(service),
            &format!("`{field}` depends on a variable with no default, so it could be `host`"),
        );
        return;
    }
    let mode = text.value.trim().to_ascii_lowercase();
    if mode == "host" {
        ctx.push(
            Severity::High,
            kind,
            Some(service),
            &format!("sets `{field}: host`, which shares that namespace with the host"),
        );
    } else if allow_refs && (mode.starts_with("service:") || mode.starts_with("container:")) {
        ctx.push(
            Severity::Medium,
            kind,
            Some(service),
            &format!("`{field}` shares a namespace with `{}`", text.value),
        );
    } else if !benign.contains(&mode.as_str()) {
        ctx.unrecognized(
            Some(service),
            &format!("`{field}` has an unrecognized value: `{}`", text.value),
        );
    } else if text.dynamic {
        ctx.push(
            Severity::Medium,
            kind,
            Some(service),
            &format!("`{field}` comes from a variable, which can change it to `host`"),
        );
    }
}

/// Items of a field that should be a list of text. Other shapes are reported,
/// and a bare scalar is still returned so that it gets checked.
fn string_items(service: &str, field: &str, value: &Value, ctx: &mut Ctx) -> Vec<String> {
    let mut items = Vec::new();
    match value {
        Value::Null => {}
        Value::Sequence(list) => {
            for item in list {
                match item {
                    Value::String(text) => items.push(text.clone()),
                    other => {
                        ctx.unrecognized(
                            Some(service),
                            &format!("`{field}` has an entry that is not text: {}", show(other)),
                        );
                        items.extend(scalar_text(other));
                    }
                }
            }
        }
        other => {
            ctx.unrecognized(
                Some(service),
                &format!("`{field}` is not a list: {}", show(other)),
            );
            items.extend(scalar_text(other));
        }
    }
    items
}

fn check_caps(service: &str, value: &Value, ctx: &mut Ctx) {
    const CRITICAL: [&str; 2] = ["ALL", "SYS_ADMIN"];
    const HIGH: [&str; 11] = [
        "BPF",
        "DAC_OVERRIDE",
        "DAC_READ_SEARCH",
        "NET_ADMIN",
        "NET_RAW",
        "PERFMON",
        "SYS_BOOT",
        "SYS_MODULE",
        "SYS_PTRACE",
        "SYS_RAWIO",
        "SYS_TIME",
    ];
    for cap in string_items(service, "cap_add", value, ctx) {
        let text = interpolate(&cap);
        let upper = text.value.trim().to_ascii_uppercase();
        let name = upper.strip_prefix("CAP_").unwrap_or(&upper);
        let severity = if text.unresolved || CRITICAL.contains(&name) {
            Severity::Critical
        } else if HIGH.contains(&name) {
            Severity::High
        } else {
            Severity::Medium
        };
        ctx.push(
            severity,
            FindingKind::CapAdd,
            Some(service),
            &format!("adds the capability `{}`", text.value),
        );
    }
}

fn check_security_opt(service: &str, value: &Value, ctx: &mut Ctx) {
    for option in string_items(service, "security_opt", value, ctx) {
        let text = interpolate(&option);
        // `:` and `=` are both accepted as the separator.
        let normal: String = text
            .value
            .to_ascii_lowercase()
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| if c == '=' { ':' } else { c })
            .collect();
        let shown = text.value.clone();
        let (severity, kind_message) = if text.unresolved {
            (
                Severity::High,
                format!("`security_opt` depends on a variable with no default: `{shown}`"),
            )
        } else if matches!(
            normal.as_str(),
            "seccomp:unconfined"
                | "apparmor:unconfined"
                | "label:disable"
                | "label:type:spc_t"
                | "systempaths:unconfined"
                | "no-new-privileges:false"
                | "no_new_privileges:false"
        ) {
            (
                Severity::High,
                format!("disables confinement with `{shown}`"),
            )
        } else if matches!(
            normal.as_str(),
            "no-new-privileges" | "no-new-privileges:true" | "no_new_privileges:true"
        ) {
            if !text.dynamic {
                continue;
            }
            (
                Severity::Medium,
                format!("`{shown}` comes from a variable, which can disable it"),
            )
        } else if normal.starts_with("seccomp:")
            || normal.starts_with("apparmor:")
            || normal.starts_with("label:")
        {
            (
                Severity::Medium,
                format!("sets a custom confinement profile: `{shown}`"),
            )
        } else {
            ctx.unrecognized(
                Some(service),
                &format!("`security_opt` has an unrecognized value: `{shown}`"),
            );
            continue;
        };
        ctx.push(
            severity,
            FindingKind::SecurityOpt,
            Some(service),
            &kind_message,
        );
    }
}

fn env_keys(service: &str, value: &Value, ctx: &mut Ctx) -> Vec<String> {
    let mut raw: Vec<String> = Vec::new();
    match value {
        Value::Null => {}
        Value::Mapping(map) => raw.extend(map.keys().map(key_text)),
        Value::Sequence(items) => {
            for item in items {
                match item.as_str() {
                    Some(text) => {
                        raw.push(
                            text.split_once('=')
                                .map_or(text, |(key, _)| key)
                                .to_string(),
                        );
                    }
                    None => ctx
                        .unrecognized(Some(service), "`environment` has an entry that is not text"),
                }
            }
        }
        _ => ctx.unrecognized(Some(service), "`environment` has an unknown shape"),
    }
    let mut keys = Vec::new();
    for key in raw {
        let bytes = key.as_bytes();
        let valid = bytes
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
            && bytes
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
        if valid {
            keys.push(key);
        } else {
            // The text is not shown, because a malformed entry may be a value.
            ctx.unrecognized(
                Some(service),
                "`environment` has a key with unexpected characters",
            );
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

/// Reports `env_file` entries. The files themselves are never read.
pub(super) fn check_env_file(service: Option<&str>, value: &Value, ctx: &mut Ctx) {
    let items: Vec<&Value> = match value {
        Value::Null => Vec::new(),
        Value::Sequence(items) => items.iter().collect(),
        other => vec![other],
    };
    for item in items {
        match item {
            Value::String(path) => report_env_file(service, path, ctx),
            Value::Mapping(map) => {
                for (key, field) in map {
                    let key = key_text(key);
                    match key.as_str() {
                        "path" => match field.as_str() {
                            Some(path) => report_env_file(service, path, ctx),
                            None => {
                                ctx.unrecognized(service, "`env_file` has a path that is not text")
                            }
                        },
                        "required" | "format" => {}
                        other => ctx.unrecognized(
                            service,
                            &format!("`env_file` has unknown field `{other}`"),
                        ),
                    }
                }
                if !map.contains_key("path") {
                    ctx.unrecognized(service, "`env_file` has an entry with no path");
                }
            }
            _ => ctx.unrecognized(service, "`env_file` has an entry of an unknown shape"),
        }
    }
}

fn report_env_file(service: Option<&str>, raw: &str, ctx: &mut Ctx) {
    let path = interpolate(raw);
    match ctx.classify(&path.value) {
        PathClass::Inside(rel) => {
            ctx.check_symlink(service, &rel);
            ctx.push(
                Severity::Medium,
                FindingKind::EnvFile,
                service,
                &format!("loads environment file {} into the service", path.value),
            );
        }
        outside => ctx.push(
            Severity::High,
            FindingKind::EnvFile,
            service,
            &format!(
                "loads environment file {} from outside the project ({})",
                path.value,
                outside.describe()
            ),
        ),
    }
}

fn check_read_file(service: &str, field: &str, raw: &str, ctx: &mut Ctx) {
    let path = interpolate(raw);
    let (severity, place) = match ctx.classify(&path.value) {
        PathClass::Inside(rel) => {
            ctx.check_symlink(Some(service), &rel);
            (Severity::Medium, "inside the project".to_string())
        }
        outside => (
            Severity::High,
            format!("outside the project ({})", outside.describe()),
        ),
    };
    ctx.push(
        severity,
        FindingKind::Weakening,
        Some(service),
        &format!("`{field}` reads {} {place}", path.value),
    );
}

fn check_build(service: &str, value: &Value, ctx: &mut Ctx) {
    match value {
        Value::String(context) => {
            check_build_path(service, "build context", context, ctx);
        }
        Value::Mapping(map) => {
            let mut context_dir = Vec::new();
            if let Some(context) = map.get("context") {
                match context.as_str() {
                    Some(text) => {
                        check_build_path(service, "build context", text, ctx);
                        if let PathClass::Inside(rel) = ctx.classify(&interpolate(text).value) {
                            context_dir = rel
                                .split('/')
                                .filter(|p| !p.is_empty())
                                .map(str::to_string)
                                .collect();
                        }
                    }
                    None => ctx.unrecognized(Some(service), "the build context is not text"),
                }
            }
            for (key, item) in map {
                let key = key_text(key);
                match key.as_str() {
                    "context" => {}
                    "dockerfile" => check_dockerfile(service, item, &context_dir, ctx),
                    "additional_contexts" => check_additional_contexts(service, item, ctx),
                    "ssh" => {
                        if !is_empty(item) {
                            ctx.push(
                                Severity::High,
                                FindingKind::BuildOption,
                                Some(service),
                                "the build forwards an SSH agent or key",
                            );
                        }
                    }
                    "privileged" => check_flag(
                        service,
                        "build.privileged",
                        item,
                        FindingKind::BuildOption,
                        Severity::Critical,
                        ctx,
                    ),
                    "entitlements" => {
                        if !is_empty(item) {
                            ctx.push(
                                Severity::High,
                                FindingKind::BuildOption,
                                Some(service),
                                "the build requests extra entitlements",
                            );
                        }
                    }
                    "network" => check_mode(
                        service,
                        "build.network",
                        item,
                        FindingKind::BuildOption,
                        &["", "default", "none", "bridge"],
                        false,
                        ctx,
                    ),
                    k if k.starts_with("x-") || BUILD_FIELDS.contains(&k) => {}
                    other => {
                        ctx.unrecognized(Some(service), &format!("unknown build field `{other}`"))
                    }
                }
            }
        }
        _ => ctx.unrecognized(Some(service), "`build` has an unknown shape"),
    }
}

fn check_build_path(service: &str, what: &str, raw: &str, ctx: &mut Ctx) {
    let path = interpolate(raw);
    if is_remote(&path.value) {
        ctx.push(
            Severity::Medium,
            FindingKind::BuildContext,
            Some(service),
            &format!("{what} comes from a remote source: {}", path.value),
        );
        return;
    }
    match ctx.classify(&path.value) {
        PathClass::Inside(rel) => {
            ctx.check_symlink(Some(service), &rel);
            if path.dynamic {
                ctx.push(
                    Severity::Medium,
                    FindingKind::BuildContext,
                    Some(service),
                    &format!(
                        "{what} {} comes from a variable, which can point it outside the project",
                        path.value
                    ),
                );
            }
        }
        outside => ctx.push(
            Severity::High,
            FindingKind::BuildContext,
            Some(service),
            &format!(
                "{what} is outside the project ({}): {}",
                outside.describe(),
                path.value
            ),
        ),
    }
}

fn check_dockerfile(service: &str, value: &Value, context_dir: &[String], ctx: &mut Ctx) {
    let Some(text) = text_value(value) else {
        ctx.unrecognized(Some(service), "the build `dockerfile` is not text");
        return;
    };
    // Relative to the build context, so `../Dockerfile` may still be inside.
    let class = super::classify_from(context_dir, &text.value);
    if !matches!(class, PathClass::Inside(_)) {
        ctx.push(
            Severity::High,
            FindingKind::BuildContext,
            Some(service),
            &format!(
                "the Dockerfile is outside the project ({}): {}",
                class.describe(),
                text.value
            ),
        );
    }
}

fn check_additional_contexts(service: &str, value: &Value, ctx: &mut Ctx) {
    let mut sources: Vec<String> = Vec::new();
    match value {
        Value::Null => {}
        Value::Mapping(map) => {
            for item in map.values() {
                match scalar_text(item) {
                    Some(text) => sources.push(text),
                    None => ctx.unrecognized(
                        Some(service),
                        "`additional_contexts` has a value that is not text",
                    ),
                }
            }
        }
        Value::Sequence(items) => {
            for item in items {
                match item.as_str().and_then(|text| text.split_once('=')) {
                    Some((_, source)) => sources.push(source.to_string()),
                    None => ctx.unrecognized(
                        Some(service),
                        "`additional_contexts` has an entry that is not name=source",
                    ),
                }
            }
        }
        _ => ctx.unrecognized(Some(service), "`additional_contexts` has an unknown shape"),
    }
    for source in sources {
        if source.starts_with("docker-image://")
            || source.starts_with("service:")
            || source.starts_with("target:")
        {
            continue;
        }
        let path = source.strip_prefix("oci-layout://").unwrap_or(&source);
        check_build_path(service, "additional build context", path, ctx);
    }
}

fn check_extends(service: &str, value: &Value, ctx: &mut Ctx) {
    match value {
        Value::String(base) => report_same_file_extends(service, base, ctx),
        Value::Mapping(map) => {
            let mut file = None;
            let mut base = None;
            for (key, item) in map {
                let key = key_text(key);
                match key.as_str() {
                    "file" => file = Some(item),
                    "service" => base = Some(item),
                    other => ctx.unrecognized(
                        Some(service),
                        &format!("`extends` has unknown field `{other}`"),
                    ),
                }
            }
            if base.is_none() {
                ctx.unrecognized(Some(service), "`extends` names no service");
            }
            match file {
                None => report_same_file_extends(
                    service,
                    base.and_then(Value::as_str).unwrap_or("<unknown>"),
                    ctx,
                ),
                Some(item) => match item.as_str().map(interpolate) {
                    None => {
                        ctx.unrecognized(Some(service), "`extends` has a file that is not text")
                    }
                    Some(path) => {
                        let place = if is_remote(&path.value) {
                            "from a remote source".to_string()
                        } else {
                            match ctx.classify(&path.value) {
                                PathClass::Inside(rel) => {
                                    ctx.check_symlink(Some(service), &rel);
                                    "inside the project".to_string()
                                }
                                outside => format!("outside the project ({})", outside.describe()),
                            }
                        };
                        ctx.push(
                            Severity::High,
                            FindingKind::Extends,
                            Some(service),
                            &format!(
                                "extends a service from {} {place}, and this check did not read that file",
                                path.value
                            ),
                        );
                    }
                },
            }
        }
        _ => ctx.unrecognized(Some(service), "`extends` has an unknown shape"),
    }
}

fn report_same_file_extends(service: &str, base: &str, ctx: &mut Ctx) {
    ctx.push(
        Severity::Info,
        FindingKind::Extends,
        Some(service),
        &format!("extends service `{base}` in this file and inherits its settings"),
    );
}

/// Checks `deploy` and returns true if it set the replica count.
fn check_deploy(service: &str, value: &Value, replicas: &mut Replicas, ctx: &mut Ctx) -> bool {
    let map = match value {
        Value::Null => return false,
        Value::Mapping(map) => map,
        _ => {
            ctx.unrecognized(Some(service), "`deploy` is not a mapping");
            return false;
        }
    };
    let mut found = false;
    for (key, item) in map {
        let key = key_text(key);
        match key.as_str() {
            "replicas" => {
                *replicas = parse_replicas(service, item, ctx);
                found = true;
            }
            k if k.starts_with("x-") || DEPLOY_FIELDS.contains(&k) => {}
            other => ctx.unrecognized(Some(service), &format!("unknown deploy field `{other}`")),
        }
    }
    found
}

fn parse_replicas(service: &str, value: &Value, ctx: &mut Ctx) -> Replicas {
    let unknown = Replicas {
        count: None,
        expression: None,
        dynamic: false,
    };
    let replicas = match value {
        Value::Number(number) => match number.as_u64().and_then(|n| u32::try_from(n).ok()) {
            Some(count) => Replicas {
                count: Some(count),
                ..unknown
            },
            None => {
                ctx.unrecognized(
                    Some(service),
                    &format!("the replica count is not valid: {}", show(value)),
                );
                unknown
            }
        },
        Value::String(raw) => {
            let text = interpolate(raw);
            let count = text.value.trim().parse::<u32>().ok();
            if count.is_none() {
                ctx.unrecognized(
                    Some(service),
                    &format!("the replica count cannot be determined: `{raw}`"),
                );
            }
            Replicas {
                count,
                expression: Some(clean(raw)),
                dynamic: text.dynamic,
            }
        }
        _ => {
            ctx.unrecognized(
                Some(service),
                &format!("the replica count is not valid: {}", show(value)),
            );
            unknown
        }
    };
    if replicas.count.is_some_and(|c| c > LARGE_REPLICA_COUNT) {
        ctx.push(
            Severity::Medium,
            FindingKind::Weakening,
            Some(service),
            &format!("asks for more than {LARGE_REPLICA_COUNT} replicas"),
        );
    }
    replicas
}

fn check_service_refs(service: &str, field: &str, value: &Value, ctx: &mut Ctx) {
    match value {
        Value::Null => {}
        Value::Sequence(items) => {
            if items
                .iter()
                .any(|item| !item.is_string() && !item.is_mapping())
            {
                ctx.unrecognized(
                    Some(service),
                    &format!("`{field}` has an entry of an unknown shape"),
                );
            }
        }
        _ => ctx.unrecognized(Some(service), &format!("`{field}` is not a list")),
    }
}

fn check_hooks(service: &str, field: &str, value: &Value, ctx: &mut Ctx) {
    let Some(items) = value.as_sequence() else {
        if !value.is_null() {
            ctx.unrecognized(Some(service), &format!("`{field}` is not a list"));
        }
        return;
    };
    for item in items {
        match item.as_mapping().and_then(|map| map.get("privileged")) {
            Some(setting) => check_flag(
                service,
                &format!("{field}.privileged"),
                setting,
                FindingKind::Privileged,
                Severity::Critical,
                ctx,
            ),
            None if item.is_mapping() => {}
            None => ctx.unrecognized(
                Some(service),
                &format!("`{field}` has an entry of an unknown shape"),
            ),
        }
    }
}

fn check_extra_hosts(service: &str, value: &Value, ctx: &mut Ctx) {
    let mentions_gateway = |text: &str| text.contains("host-gateway");
    let has_gateway = match value {
        Value::Null => false,
        Value::Sequence(items) => {
            let bad = items.iter().any(|item| !item.is_string());
            if bad {
                ctx.unrecognized(Some(service), "`extra_hosts` has an entry that is not text");
            }
            items.iter().filter_map(Value::as_str).any(mentions_gateway)
        }
        Value::Mapping(map) => map.values().filter_map(Value::as_str).any(mentions_gateway),
        _ => {
            ctx.unrecognized(Some(service), "`extra_hosts` has an unknown shape");
            false
        }
    };
    if has_gateway {
        ctx.push(
            Severity::Medium,
            FindingKind::Weakening,
            Some(service),
            "maps a name to `host-gateway`, which points at the machine running the containers",
        );
    }
}
