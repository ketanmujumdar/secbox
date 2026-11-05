//! Reads a Compose file as data. Docker is never called, and variables are
//! resolved from their written defaults, never from the host or a `.env` file.
//!
//! This fails closed: a field that is not understood becomes a finding.

mod interp;
mod ports;
mod service;
mod volumes;

use std::collections::BTreeMap;
use std::path::{Component, Path};

use serde::Serialize;
use serde_norway::Value;

use crate::detect::COMPOSE_OVERRIDE_FILES;
use crate::safe_fs::{read_inside, resolve_inside, Entry};
use crate::{clean, yaml, Error};

pub use interp::UNKNOWN_VALUE;
use interp::{interpolate, Interpolated};

/// The largest Compose file that will be parsed.
pub const MAX_COMPOSE_BYTES: u64 = 1024 * 1024;

const MAX_FINDINGS: usize = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Severity {
    Info,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum FindingKind {
    Privileged,
    NetworkMode,
    PidMode,
    IpcMode,
    UtsMode,
    UsernsMode,
    CgroupMode,
    CapAdd,
    SecurityOpt,
    Device,
    DockerSocket,
    /// A bind mount from outside the project, or from an unknown place.
    HostMount,
    /// A relative bind mount that stays inside the project.
    ProjectMount,
    /// A project path that is a link leading out of the project.
    SymlinkEscape,
    /// A mount of an environment file, which may hold secrets.
    EnvFileMount,
    PortAllInterfaces,
    PortLoopback,
    PortSpecificInterface,
    /// The published interface is known only at start time.
    PortUnknownInterface,
    Secret,
    Config,
    BuildContext,
    /// A build setting that weakens isolation, such as SSH agent forwarding.
    BuildOption,
    EnvFile,
    Include,
    Extends,
    Profile,
    /// A top-level volume or network that reaches outside the stack.
    TopLevelResource,
    /// An override file that Compose would merge automatically.
    OverrideFile,
    /// A setting that weakens isolation and has no kind of its own.
    Weakening,
    /// A shape or field this check does not understand.
    Unrecognized,
}

/// One thing the Compose file asks for. All text is safe to print.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub severity: Severity,
    pub kind: FindingKind,
    pub service: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComposeReport {
    pub findings: Vec<Finding>,
    pub model: ComposeModel,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ComposeModel {
    /// In file order.
    pub services: Vec<Service>,
    /// Environment keys used by more than one service, sorted by key.
    pub shared_env: Vec<SharedEnv>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Service {
    pub name: String,
    pub image: Option<String>,
    pub has_build: bool,
    pub replicas: Replicas,
    /// Empty means the service starts by default.
    pub profiles: Vec<String>,
    pub ports: Vec<Port>,
    pub mounts: Vec<Mount>,
    /// Names only. Values are never kept, because they may be secrets.
    pub env_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Replicas {
    /// The count with defaults applied, or `None` when it cannot be known.
    pub count: Option<u32>,
    /// The expression as written, when it was not a plain number.
    pub expression: Option<String>,
    /// A variable can change the count at start time.
    pub dynamic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Port {
    /// First container port, or `None` when it cannot be known.
    pub target: Option<u16>,
    /// Last container port when a range is published.
    pub target_end: Option<u16>,
    /// Host port or range as written with defaults. `None` means Docker
    /// chooses one.
    pub published: Option<String>,
    pub interface: Interface,
    pub protocol: String,
    /// A variable can change this entry at start time.
    pub dynamic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Interface {
    /// All interfaces, including when none is written.
    All,
    Loopback,
    Specific(String),
    /// Not knowable statically, or not a valid address. Treat it as `All`.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MountKind {
    /// A relative path that stays inside the project.
    Project,
    /// A path outside the project, or one that cannot be known.
    Host,
    Named,
    Anonymous,
    Tmpfs,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Mount {
    pub kind: MountKind,
    pub source: Option<String>,
    pub target: Option<String>,
    pub read_only: bool,
    /// A variable can change this mount at start time.
    pub dynamic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SharedEnv {
    pub key: String,
    /// Sorted, with at least two entries.
    pub services: Vec<String>,
}

impl ComposeModel {
    pub fn service(&self, name: &str) -> Option<&Service> {
        self.services.iter().find(|s| s.name == name)
    }

    /// Services that start with no profile selected.
    pub fn default_services(&self) -> impl Iterator<Item = &Service> {
        self.services.iter().filter(|s| s.profiles.is_empty())
    }

    /// Port entries of default services published on all interfaces.
    pub fn default_ports_on_all_interfaces(&self) -> usize {
        self.default_services()
            .flat_map(|s| &s.ports)
            .filter(|p| p.interface == Interface::All)
            .count()
    }
}

/// Analyzes Compose text as if the file were at the project root, without
/// touching the disk. Use [`parse_file`] to also check links in the project.
pub fn parse_str(text: &str) -> Result<ComposeReport, Error> {
    analyze(text, Vec::new(), None)
}

/// Analyzes one Compose file. `file` is relative to `root` and must stay
/// inside it. Environment files are refused.
pub fn parse_file(root: &Path, file: &Path) -> Result<ComposeReport, Error> {
    let text = read_inside(root, file, MAX_COMPOSE_BYTES)?;
    analyze(&text, base_dir(file), Some(root))
}

/// Analyzes a Compose file and any override file next to it, which Compose
/// would merge without being asked. Override findings are added to the report
/// and marked. The model describes the main file only.
pub fn check_project(root: &Path, file: &Path) -> Result<ComposeReport, Error> {
    let mut report = parse_file(root, file)?;
    let dir = file.parent().unwrap_or(Path::new(""));
    for name in COMPOSE_OVERRIDE_FILES {
        let path = dir.join(name);
        if resolve_inside(root, &path)? == Entry::Missing {
            continue;
        }
        report.findings.push(finding(
            Severity::High,
            FindingKind::OverrideFile,
            None,
            &format!(
                "{name} would be merged into this stack automatically, and the service list above does not include its changes"
            ),
        ));
        match parse_file(root, &path) {
            Ok(extra) => report.findings.extend(extra.findings.into_iter().map(|f| {
                let message = format!("[{name}] {}", f.message);
                Finding { message, ..f }
            })),
            Err(err) => report.findings.push(finding(
                Severity::Critical,
                FindingKind::Unrecognized,
                None,
                &format!("{name} could not be checked: {err}"),
            )),
        }
    }
    Ok(report)
}

fn base_dir(file: &Path) -> Vec<String> {
    let mut base = Vec::new();
    if let Some(parent) = file.parent() {
        for part in parent.components() {
            match part {
                Component::Normal(name) => base.push(name.to_string_lossy().into_owned()),
                Component::ParentDir => {
                    base.pop();
                }
                _ => {}
            }
        }
    }
    base
}

fn analyze(text: &str, base: Vec<String>, root: Option<&Path>) -> Result<ComposeReport, Error> {
    if text.len() as u64 > MAX_COMPOSE_BYTES {
        return Err(Error::InputTooLarge {
            limit: MAX_COMPOSE_BYTES,
        });
    }
    let mut doc = yaml::load(text)?;
    if !doc.is_mapping() {
        return Err(Error::NotAMapping);
    }
    let mut ctx = Ctx {
        base,
        root,
        findings: Vec::new(),
        truncated: false,
    };
    strip_tags(&mut doc, &mut Vec::new(), &mut ctx);
    let model = analyze_document(&doc, &mut ctx);
    if ctx.truncated {
        ctx.findings.push(finding(
            Severity::Critical,
            FindingKind::Unrecognized,
            None,
            &format!("more than {MAX_FINDINGS} findings: the rest were not listed"),
        ));
    }
    Ok(ComposeReport {
        findings: ctx.findings,
        model,
    })
}

fn analyze_document(doc: &Value, ctx: &mut Ctx) -> ComposeModel {
    let mut model = ComposeModel::default();
    let Some(map) = doc.as_mapping() else {
        return model;
    };
    let mut saw_services = false;
    for (key, value) in map {
        let name = key.as_str().unwrap_or("<not a string>");
        if name.starts_with("x-") {
            continue;
        }
        match name {
            "services" => {
                saw_services = true;
                model.services = analyze_services(value, ctx);
            }
            "version" | "name" | "models" => {}
            "networks" => check_networks(value, ctx),
            "volumes" => check_volumes(value, ctx),
            "secrets" => check_file_objects(value, "secret", FindingKind::Secret, ctx),
            "configs" => check_file_objects(value, "config", FindingKind::Config, ctx),
            "include" => check_include(value, ctx),
            other => ctx.unrecognized(None, &format!("unknown top-level field `{other}`")),
        }
    }
    if !saw_services && !map.contains_key("include") {
        ctx.unrecognized(None, "the file has no `services` section");
    }
    model.shared_env = shared_env(&model.services);
    model
}

fn analyze_services(value: &Value, ctx: &mut Ctx) -> Vec<Service> {
    match value {
        Value::Null => Vec::new(),
        Value::Mapping(map) => map
            .iter()
            .map(|(key, body)| service::analyze(&key_text(key), body, ctx))
            .collect(),
        _ => {
            ctx.unrecognized(None, "`services` is not a mapping");
            Vec::new()
        }
    }
}

fn shared_env(services: &[Service]) -> Vec<SharedEnv> {
    let mut by_key: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for service in services {
        for key in &service.env_keys {
            by_key.entry(key).or_default().push(service.name.clone());
        }
    }
    by_key
        .into_iter()
        .filter_map(|(key, mut names)| {
            names.sort();
            names.dedup();
            (names.len() > 1).then(|| SharedEnv {
                key: key.to_string(),
                services: names,
            })
        })
        .collect()
}

/// Reports every YAML tag, then removes it so the value under it is still
/// checked. Compose gives `!reset` and `!override` meaning across files, and
/// a single file cannot show their effect.
fn strip_tags(value: &mut Value, path: &mut Vec<String>, ctx: &mut Ctx) {
    if let Value::Tagged(tagged) = value {
        let service = match path.as_slice() {
            [first, name, ..] if first == "services" => Some(name.clone()),
            _ => None,
        };
        let message = format!("YAML tag {} at {}", tagged.tag, path.join("."));
        ctx.unrecognized(service.as_deref(), &message);
        let inner = std::mem::take(&mut tagged.value);
        *value = inner;
        strip_tags(value, path, ctx);
        return;
    }
    match value {
        Value::Mapping(map) => {
            for (key, child) in map.iter_mut() {
                let name = key_text(key);
                if path.is_empty() && name.starts_with("x-") {
                    continue;
                }
                path.push(name);
                strip_tags(child, path, ctx);
                path.pop();
            }
        }
        Value::Sequence(items) => {
            for (index, child) in items.iter_mut().enumerate() {
                path.push(index.to_string());
                strip_tags(child, path, ctx);
                path.pop();
            }
        }
        _ => {}
    }
}

fn check_networks(value: &Value, ctx: &mut Ctx) {
    for (name, body) in named_objects(value, "networks", ctx) {
        let Some(map) = body.as_mapping() else {
            if !body.is_null() {
                ctx.unrecognized(None, &format!("network `{name}` is not a mapping"));
            }
            continue;
        };
        for (key, item) in map {
            let key = key_text(key);
            match key.as_str() {
                "driver" => {
                    let driver = item.as_str().map(interpolate);
                    match driver.as_ref().map(|d| (d.value.as_str(), d.dynamic)) {
                        Some(("bridge" | "overlay" | "default", false)) => {}
                        _ => ctx.push(
                            Severity::High,
                            FindingKind::TopLevelResource,
                            None,
                            &format!("network `{name}` uses a driver that may reach the host network: {}", show(item)),
                        ),
                    }
                }
                "name" => {
                    let actual = item.as_str().map(interpolate);
                    let suspicious = actual
                        .as_ref()
                        .is_none_or(|n| n.unresolved || n.value == "host" || n.value == "none");
                    if suspicious {
                        ctx.push(
                            Severity::High,
                            FindingKind::TopLevelResource,
                            None,
                            &format!("network `{name}` is named {}", show(item)),
                        );
                    }
                }
                "external" => {
                    if !matches!(service::flag(item), service::Flag::Off) {
                        ctx.push(
                            Severity::Medium,
                            FindingKind::TopLevelResource,
                            None,
                            &format!("network `{name}` is external to this stack"),
                        );
                    }
                }
                "driver_opts" | "ipam" | "internal" | "attachable" | "enable_ipv4"
                | "enable_ipv6" | "labels" => {}
                other if other.starts_with("x-") => {}
                other => ctx.unrecognized(
                    None,
                    &format!("network `{name}` has unknown field `{other}`"),
                ),
            }
        }
    }
}

fn check_volumes(value: &Value, ctx: &mut Ctx) {
    for (name, body) in named_objects(value, "volumes", ctx) {
        let Some(map) = body.as_mapping() else {
            if !body.is_null() {
                ctx.unrecognized(None, &format!("volume `{name}` is not a mapping"));
            }
            continue;
        };
        for (key, item) in map {
            let key = key_text(key);
            match key.as_str() {
                "driver_opts" => ctx.push(
                    Severity::High,
                    FindingKind::HostMount,
                    None,
                    &format!("volume `{name}` sets driver options, which can bind a host path or remote share"),
                ),
                "driver" => {
                    if item.as_str() != Some("local") {
                        ctx.push(
                            Severity::Medium,
                            FindingKind::TopLevelResource,
                            None,
                            &format!("volume `{name}` uses driver {}", show(item)),
                        );
                    }
                }
                "external" => {
                    if !matches!(service::flag(item), service::Flag::Off) {
                        ctx.push(
                            Severity::Medium,
                            FindingKind::TopLevelResource,
                            None,
                            &format!("volume `{name}` is external to this stack"),
                        );
                    }
                }
                "name" | "labels" => {}
                other if other.starts_with("x-") => {}
                other => ctx.unrecognized(
                    None,
                    &format!("volume `{name}` has unknown field `{other}`"),
                ),
            }
        }
    }
}

/// Checks top-level `secrets` or `configs`.
fn check_file_objects(value: &Value, label: &str, kind: FindingKind, ctx: &mut Ctx) {
    let section = format!("{label}s");
    for (name, body) in named_objects(value, &section, ctx) {
        let Some(map) = body.as_mapping() else {
            ctx.unrecognized(None, &format!("{label} `{name}` is not a mapping"));
            continue;
        };
        let mut has_source = false;
        for (key, item) in map {
            let key = key_text(key);
            match key.as_str() {
                "file" => {
                    has_source = true;
                    check_object_file(&name, label, kind, item, ctx);
                }
                "environment" => {
                    has_source = true;
                    ctx.push(
                        Severity::Medium,
                        kind,
                        None,
                        &format!(
                            "{label} `{name}` takes its value from a host environment variable"
                        ),
                    );
                }
                "external" => {
                    has_source = true;
                    if !matches!(service::flag(item), service::Flag::Off) {
                        ctx.push(
                            Severity::Medium,
                            kind,
                            None,
                            &format!("{label} `{name}` is external to this stack"),
                        );
                    }
                }
                "content" => has_source = true,
                "name" | "driver" | "driver_opts" | "template_driver" | "labels" => {}
                other if other.starts_with("x-") => {}
                other => ctx.unrecognized(
                    None,
                    &format!("{label} `{name}` has unknown field `{other}`"),
                ),
            }
        }
        if !has_source {
            ctx.unrecognized(None, &format!("{label} `{name}` names no source"));
        }
    }
}

fn check_object_file(name: &str, label: &str, kind: FindingKind, item: &Value, ctx: &mut Ctx) {
    let Some(raw) = item.as_str() else {
        ctx.unrecognized(
            None,
            &format!("{label} `{name}` has a `file` that is not text"),
        );
        return;
    };
    let path = interpolate(raw);
    let class = ctx.classify(&path.value);
    match class {
        PathClass::Inside(rel) => {
            ctx.check_symlink(None, &rel);
            let (severity, note) = if path.dynamic {
                (Severity::Medium, ", but a variable can change the path")
            } else {
                (Severity::Info, "")
            };
            ctx.push(
                severity,
                kind,
                None,
                &format!(
                    "{label} `{name}` reads the project file {}{note}",
                    path.value
                ),
            );
        }
        other => ctx.push(
            Severity::High,
            kind,
            None,
            &format!(
                "{label} `{name}` reads a file outside the project ({}): {}",
                other.describe(),
                path.value
            ),
        ),
    }
}

fn check_include(value: &Value, ctx: &mut Ctx) {
    let items: Vec<&Value> = match value {
        Value::Null => Vec::new(),
        Value::Sequence(items) => items.iter().collect(),
        other => {
            ctx.unrecognized(None, "`include` is not a list");
            vec![other]
        }
    };
    for item in items {
        match item {
            Value::String(path) => report_include(path, ctx),
            Value::Mapping(map) => {
                for (key, field) in map {
                    let key = key_text(key);
                    match key.as_str() {
                        "path" => match field {
                            Value::String(path) => report_include(path, ctx),
                            Value::Sequence(paths) => {
                                for path in paths {
                                    match path.as_str() {
                                        Some(path) => report_include(path, ctx),
                                        None => ctx.unrecognized(
                                            None,
                                            "`include` has a path that is not text",
                                        ),
                                    }
                                }
                            }
                            _ => ctx.unrecognized(None, "`include` has a path that is not text"),
                        },
                        "project_directory" => {
                            let dir = field.as_str().map(interpolate);
                            let outside = dir.as_ref().is_none_or(|d| {
                                !matches!(ctx.classify(&d.value), PathClass::Inside(_))
                            });
                            if outside {
                                ctx.push(
                                    Severity::High,
                                    FindingKind::Include,
                                    None,
                                    &format!("`include` uses a project directory outside the project: {}", show(field)),
                                );
                            }
                        }
                        "env_file" => service::check_env_file(None, field, ctx),
                        other => ctx
                            .unrecognized(None, &format!("`include` has unknown field `{other}`")),
                    }
                }
            }
            _ => ctx.unrecognized(None, "`include` has an entry of an unknown shape"),
        }
    }
}

fn report_include(raw: &str, ctx: &mut Ctx) {
    let path = interpolate(raw);
    let place = if is_remote(&path.value) {
        "from a remote source".to_string()
    } else {
        match ctx.classify(&path.value) {
            PathClass::Inside(rel) => {
                ctx.check_symlink(None, &rel);
                "inside the project".to_string()
            }
            other => format!("outside the project ({})", other.describe()),
        }
    };
    ctx.push(
        Severity::High,
        FindingKind::Include,
        None,
        &format!(
            "`include` pulls in {} {place}, and this check did not read that file",
            path.value
        ),
    );
}

fn named_objects<'v>(value: &'v Value, section: &str, ctx: &mut Ctx) -> Vec<(String, &'v Value)> {
    match value {
        Value::Null => Vec::new(),
        Value::Mapping(map) => map.iter().map(|(k, v)| (key_text(k), v)).collect(),
        _ => {
            ctx.unrecognized(None, &format!("`{section}` is not a mapping"));
            Vec::new()
        }
    }
}

/// True for a URL-like source that does not name a local path.
fn is_remote(path: &str) -> bool {
    path.contains("://")
        || path.starts_with("git@")
        || path.ends_with(".git")
        || path.contains(".git#")
}

/// The text of a mapping key. A key that is not a scalar is still analyzed.
fn key_text(key: &Value) -> String {
    scalar_text(key).unwrap_or_else(|| "<not a string>".to_string())
}

fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// A value in a message. Only ever used on fields that do not hold secrets.
fn show(value: &Value) -> String {
    match scalar_text(value) {
        Some(text) => format!("`{text}`"),
        None if value.is_null() => "`null`".to_string(),
        None => "a value of an unknown shape".to_string(),
    }
}

/// Where a path points, judged from its text.
enum PathClass {
    /// Inside the project, as a normalized project-relative path.
    Inside(String),
    Absolute,
    Home,
    Escapes,
    Unknown,
}

impl PathClass {
    fn describe(&self) -> &'static str {
        match self {
            PathClass::Inside(_) => "inside the project",
            PathClass::Absolute => "absolute path",
            PathClass::Home => "home directory",
            PathClass::Escapes => "`..` leaves the project",
            PathClass::Unknown => "depends on a variable with no default",
        }
    }
}

/// Resolves `..` in the text of a path, starting from `base`.
fn classify_from(base: &[String], raw: &str) -> PathClass {
    if raw.contains(UNKNOWN_VALUE) || raw.is_empty() {
        return PathClass::Unknown;
    }
    let path = raw.replace('\\', "/");
    if path.starts_with('~') {
        return PathClass::Home;
    }
    let bytes = path.as_bytes();
    let has_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if path.starts_with('/') || has_drive {
        return PathClass::Absolute;
    }
    let mut stack: Vec<&str> = base.iter().map(String::as_str).collect();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if stack.pop().is_none() {
                    return PathClass::Escapes;
                }
            }
            name => stack.push(name),
        }
    }
    PathClass::Inside(stack.join("/"))
}

struct Ctx<'r> {
    /// The Compose file's directory, relative to the project root.
    base: Vec<String>,
    /// Set when the project is on disk, so links can be checked.
    root: Option<&'r Path>,
    findings: Vec<Finding>,
    truncated: bool,
}

impl Ctx<'_> {
    fn push(
        &mut self,
        severity: Severity,
        kind: FindingKind,
        service: Option<&str>,
        message: &str,
    ) {
        if self.findings.len() >= MAX_FINDINGS {
            self.truncated = true;
            return;
        }
        self.findings
            .push(finding(severity, kind, service, message));
    }

    fn unrecognized(&mut self, service: Option<&str>, message: &str) {
        self.push(Severity::High, FindingKind::Unrecognized, service, message);
    }

    fn classify(&self, raw: &str) -> PathClass {
        classify_from(&self.base, raw)
    }

    /// Reports a project path that is really a link out of the project.
    fn check_symlink(&mut self, service: Option<&str>, rel: &str) {
        let Some(root) = self.root else { return };
        if let Ok(Entry::Escapes) = resolve_inside(root, Path::new(rel)) {
            self.push(
                Severity::High,
                FindingKind::SymlinkEscape,
                service,
                &format!("{rel} is inside the project but links out of it, or is a broken link"),
            );
        }
    }
}

fn finding(severity: Severity, kind: FindingKind, service: Option<&str>, message: &str) -> Finding {
    Finding {
        severity,
        kind,
        service: service.map(clean),
        message: clean(message),
    }
}

/// Interpolated text and whether a variable can change it, for field checks.
fn text_value(value: &Value) -> Option<Interpolated> {
    scalar_text(value).map(|text| interpolate(&text))
}
