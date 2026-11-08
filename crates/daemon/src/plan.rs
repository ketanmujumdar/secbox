//! Working out what starting a project would do, without doing any of it.
//!
//! A plan is the thing a person approves. It is built here, on the host, from
//! the project's own files and the person's stored settings — never from
//! anything a client sent — and it is hashed, so the approval that comes back
//! can be checked against the plan that was shown.

use proto::{
    ExcludedFile, Finding, NetworkPlan, PlanReport, PortPlan, ResourcePlan, SecretPlan,
    SecretRoute, SnapshotPlan, Source, TlsPlan,
};
use proto::{text, ApiError};

use std::path::{Path, PathBuf};

use projectcheck::compose::{
    check_project, parse_str, ComposeModel, Interface, Service as ComposeService,
};
use projectcheck::detect::{detect, ProjectKind, COMPOSE_OVERRIDE_FILES};
use projectcheck::dockerfile;
use projectcheck::resources::{self, ByteSize, HostResources, MilliCpus, ResolvedResources, ResourceError};
use secrets::Delivery;

use crate::settings::{Approvals, SandboxRecord, UserSettings};
use crate::{bad, internal, unsupported, Project, Service};

/// Every host the engine spike measured a real `demo_shop` build needing.
/// Each project now gets only the groups its files point to: see
/// [`crate::buildhosts`]. This full list is what the menu is made of.
pub fn every_build_host() -> Vec<&'static str> {
    crate::buildhosts::ALL
        .iter()
        .flat_map(|g| g.hosts.iter().copied())
        .collect()
}

/// The smallest memory a build is allowed to start with. The engine spike
/// built this project at 3G and watched 2G fail all three times, killed by
/// the out-of-memory killer part way through a compile.
pub const MIN_BUILD_MEMORY_BYTES: u64 = 3 * (1 << 30);

/// The names of the services in a project's Compose file, or the one the
/// host writes for a project with only a Dockerfile, in file order.
pub fn service_names(project: &Project) -> Result<Vec<String>, ApiError> {
    let detection = detect(&project.root)
        .map_err(|e| bad(format!("cannot read the project: {}", text::escape(&e.to_string()))))?;
    let mut ignored = Vec::new();
    let files = match &detection.kind {
        ProjectKind::Compose { file, overrides } => {
            compose_project(project, file, overrides, &mut ignored)?
        }
        ProjectKind::Dockerfile { file } => dockerfile_project(project, file, &mut ignored)?,
        _ => {
            return Err(unsupported(
                "this project has no Compose file or Dockerfile, so it has no services to give a secret to",
            ))
        }
    };
    Ok(files.model.services.iter().map(|s| s.name.clone()).collect())
}

pub fn build(
    service: &Service,
    project: &Project,
    source: &Source,
) -> Result<PlanReport, ApiError> {
    let detection = detect(&project.root)
        .map_err(|e| bad(format!("cannot read the project: {}", text::escape(&e.to_string()))))?;
    // A plan that could never be snapshotted is not shown, so nothing is
    // asked about it first.
    service
        .deps()
        .check_source(&project.root, source)
        .map_err(|reason| bad(text::escape(&reason)))?;

    let mut blockers: Vec<String> = Vec::new();
    let files = match &detection.kind {
        // Detection picks a Compose file over a Dockerfile, so a project with
        // both runs its own Compose file.
        ProjectKind::Compose { file, overrides } => {
            compose_project(project, file, overrides, &mut blockers)?
        }
        ProjectKind::Dockerfile { file } => dockerfile_project(project, file, &mut blockers)?,
        ProjectKind::Unsupported { reason, .. } => {
            return Err(unsupported(format!(
                "this project needs macOS, so a Linux sandbox cannot protect it: {}",
                text::escape(reason)
            )))
        }
        _ => {
            return Err(unsupported(
                "only Compose projects and projects with a Dockerfile can be started. Other kinds come after the first release",
            ))
        }
    };

    let default_services: Vec<&ComposeService> = files.model.default_services().collect();
    let services: Vec<String> = default_services
        .iter()
        .map(|s| text::escape(&s.name))
        .collect();

    let mut ports = plan_ports(&default_services, &mut blockers);
    if files.generated_compose.is_some() {
        // The host chose to publish these on every interface inside the
        // sandbox. The project itself asked for nothing.
        for port in &mut ports {
            port.was_all_interfaces = false;
        }
    }
    crate::portset::fill_plan(
        &mut ports,
        &Approvals::load(&project.dir).map_err(internal)?,
        SandboxRecord::load(&project.dir).map_err(internal)?.is_some(),
    );
    let resources = plan_resources(service, project, &files.model, &mut blockers)?;
    let secrets = plan_secrets(project, &mut blockers)?;
    let network = plan_network(project, &secrets, &mut blockers)?;

    block_escapes_when_secrets_are_held(&secrets, &files.findings, &mut blockers);

    let findings: Vec<Finding> = files
        .findings
        .iter()
        .map(|f| Finding {
            severity: format!("{:?}", f.severity),
            service: f.service.as_deref().map(text::escape),
            message: text::escape(&f.message),
        })
        .collect();

    let snapshot = SnapshotPlan {
        revision: match source {
            Source::Commit { revision } => Some(text::escape_short(revision, 200)),
            Source::WorkingTree => None,
        },
        counts: None,
        excluded: Vec::<ExcludedFile>::new(),
        warnings: Vec::new(),
    };

    let mut plan = PlanReport {
        project_id: project.id.as_str().to_owned(),
        project_path: text::escape_path(&project.root),
        digest: String::new(),
        kind: files.kind.to_owned(),
        compose_files: files.compose_files,
        services,
        ports,
        resources,
        network,
        secrets,
        snapshot,
        findings,
        blockers,
        generated_compose: files.generated_compose,
    };
    plan.digest = digest(&plan)?;
    Ok(plan)
}

/// What a project's own files say it runs, whichever kind it is.
struct ProjectFiles {
    kind: &'static str,
    compose_files: Vec<String>,
    model: ComposeModel,
    findings: Vec<projectcheck::compose::Finding>,
    generated_compose: Option<String>,
}

fn compose_project(
    project: &Project,
    compose_file: &Path,
    overrides: &[PathBuf],
    blockers: &mut Vec<String>,
) -> Result<ProjectFiles, ApiError> {
    let report = check_project(&project.root, compose_file).map_err(|e| {
        bad(format!(
            "cannot read the Compose file: {}",
            text::escape(&e.to_string())
        ))
    })?;
    // Every file Compose would merge on its own, in the same order, so that
    // naming them explicitly does not change what the project asks for.
    let mut compose_files = Vec::new();
    for file in std::iter::once(compose_file).chain(overrides.iter().map(PathBuf::as_path)) {
        match relative_name(file) {
            Some(name) => compose_files.push(name),
            None => blockers.push(format!(
                "a Compose file is not inside the project: {}",
                text::escape_path(file)
            )),
        }
    }
    Ok(ProjectFiles {
        kind: "compose",
        compose_files,
        model: report.model,
        findings: report.findings,
        generated_compose: None,
    })
}

/// A project with a Dockerfile and no Compose file runs as a one-service
/// Compose project that the host writes. Its ports come from the Dockerfile's
/// `EXPOSE` lines and go through the same planning and approval as a Compose
/// project's: none is forwarded unless a person approves it.
///
/// The generated text is read back with the same Compose checks, so the
/// services, ports and build memory in the plan come from exactly the text
/// that will run. The text itself is in the plan, so the digest, and with it
/// the approval, covers it. Its own findings are not shown, because they
/// would describe the host's file rather than the project; the Dockerfile's
/// are.
///
/// Where the file goes is decided in `run`, at
/// [`crate::run::GENERATED_COMPOSE_FILE`].
fn dockerfile_project(
    project: &Project,
    file: &Path,
    blockers: &mut Vec<String>,
) -> Result<ProjectFiles, ApiError> {
    let report = dockerfile::check_dockerfile(&project.root, file).map_err(|e| {
        bad(format!(
            "cannot read the Dockerfile: {}",
            text::escape(&e.to_string())
        ))
    })?;
    let generated = dockerfile::generate_compose(file, &report.exposed).map_err(|e| {
        bad(format!(
            "cannot run this Dockerfile: {}",
            text::escape(&e.to_string())
        ))
    })?;
    let model = parse_str(&generated)
        .map_err(|_| internal("the generated Compose file does not read back"))?
        .model;

    // Compose merges any of these into the generated file in the commands
    // that find their files by name, such as `logs` and `exec`. One with no
    // main file to change is not something to guess about.
    for name in COMPOSE_OVERRIDE_FILES {
        if std::fs::symlink_metadata(project.root.join(name)).is_ok() {
            blockers.push(format!(
                "{name} is in the project, but there is no Compose file for it to change. Remove it, or add a Compose file."
            ));
        }
    }
    Ok(ProjectFiles {
        kind: "dockerfile",
        compose_files: vec![crate::run::GENERATED_COMPOSE_FILE.to_owned()],
        model,
        findings: report.findings,
        generated_compose: Some(generated),
    })
}

/// Settings that let a container reach past its own walls inside the VM.
///
/// Every service runs in the same VM, and the secrets assigned to any of them
/// sit there on a `tmpfs`. A container that can mount the VM's folders, talk
/// to its Docker daemon, or share its process, network or device namespaces
/// can read another service's secrets, from the files or from that service's
/// environment. Without secrets these only reach the VM, which is disposable,
/// so they stay findings. With secrets they are how one hostile service takes
/// another's keys, so a start is refused outright, with no way to approve it.
fn is_escape(finding: &projectcheck::compose::Finding) -> bool {
    use projectcheck::compose::{FindingKind as K, Severity};
    match finding.kind {
        K::Privileged
        | K::NetworkMode
        | K::PidMode
        | K::IpcMode
        | K::UsernsMode
        | K::CgroupMode
        | K::CapAdd
        | K::SecurityOpt
        | K::Device
        | K::DockerSocket
        | K::HostMount
        | K::SymlinkEscape
        | K::Weakening
        // A shape the check cannot read could be hiding any of the above.
        | K::Unrecognized => true,
        // A network driver or include that may reach outside the stack.
        K::TopLevelResource | K::Include | K::Extends => {
            matches!(finding.severity, Severity::Critical | Severity::High)
        }
        // Something that could not be checked at all.
        _ => matches!(finding.severity, Severity::Critical),
    }
}

fn block_escapes_when_secrets_are_held(
    secrets: &[SecretPlan],
    findings: &[projectcheck::compose::Finding],
    blockers: &mut Vec<String>,
) {
    if secrets.is_empty() {
        return;
    }
    for finding in findings.iter().filter(|f| is_escape(f)) {
        blockers.push(format!(
            "this project has secrets, and {} could reach past its container to them: {}. Remove that setting, or remove the secrets from this project.",
            finding
                .service
                .as_deref()
                .map_or_else(|| "the Compose file".to_owned(), |s| format!("service {}", text::escape(s))),
            text::escape(&finding.message)
        ));
    }
}

/// A Compose file's path as it will be named inside the sandbox. Anything
/// absolute or climbing out of the project is refused rather than repaired.
fn relative_name(path: &std::path::Path) -> Option<String> {
    if path.is_absolute() {
        return None;
    }
    let name = path.to_str()?;
    if name.is_empty() || name.split('/').any(|part| part == ".." || part.is_empty()) {
        return None;
    }
    Some(name.to_owned())
}

/// A stable hash of everything in the plan but the hash itself. An approval
/// names this, so a project file edited after the plan was shown cannot be
/// started under the earlier approval.
pub fn digest(plan: &PlanReport) -> Result<String, ApiError> {
    use sha2::{Digest, Sha256};
    let mut bare = plan.clone();
    bare.digest = String::new();
    let bytes = serde_json::to_vec(&bare).map_err(|_| internal("cannot hash the plan"))?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn plan_ports(services: &[&ComposeService], blockers: &mut Vec<String>) -> Vec<PortPlan> {
    let mut ports: Vec<PortPlan> = Vec::new();
    for service in services {
        for port in &service.ports {
            let name = text::escape(&service.name);
            let Some(published) = port.published.as_deref() else {
                blockers.push(format!(
                    "{name} publishes a port without saying which one, so there is nothing to approve. Give it a fixed host port."
                ));
                continue;
            };
            let Ok(host_port) = published.parse::<u16>() else {
                blockers.push(format!(
                    "{name} publishes {}, which is not one fixed port. Ranges and variables cannot be approved.",
                    text::escape_short(published, 60)
                ));
                continue;
            };
            if host_port == 0 {
                blockers.push(format!("{name} publishes port 0."));
                continue;
            }
            let Some(container_port) = port.target else {
                blockers.push(format!(
                    "{name} does not say which container port {host_port} reaches."
                ));
                continue;
            };
            if port.target_end.is_some() {
                blockers.push(format!(
                    "{name} publishes a range of container ports on {host_port}, which is not one fixed port. Ranges cannot be approved."
                ));
                continue;
            }
            // The engine forwards TCP only, and every published port is
            // rewritten for the forward (see `crate::ports`), so one this
            // could not forward is not quietly turned into a TCP port.
            if port.protocol != "tcp" {
                blockers.push(format!(
                    "{name} publishes {host_port}/{}, and only TCP ports can be forwarded. Remove it, or publish it over TCP.",
                    text::escape_short(&port.protocol, 20)
                ));
                continue;
            }
            if let Some(other) = ports.iter().find(|p| p.host_port == host_port) {
                blockers.push(format!(
                    "{name} and {} both want host port {host_port}.",
                    text::escape(&other.service)
                ));
                continue;
            }
            ports.push(PortPlan {
                service: name,
                host_port,
                container_port,
                mac_port: host_port,
                chosen: None,
                was_all_interfaces: matches!(port.interface, Interface::All | Interface::Unknown),
                dynamic: port.dynamic,
            });
        }
    }
    ports.sort_by_key(|p| p.host_port);
    ports
}

fn plan_resources(
    service: &Service,
    project: &Project,
    model: &ComposeModel,
    blockers: &mut Vec<String>,
) -> Result<ResourcePlan, ApiError> {
    let chosen = Approvals::load(&project.dir).map_err(internal)?.resources;
    let settings = UserSettings::load(service.state_dir()).map_err(internal)?;
    if host_now(service, project, &settings).available_memory == 0 {
        blockers.push("cannot tell how much memory this machine has free.".to_owned());
    }

    let resolved = match resolve_with(service, project, &settings, &chosen)? {
        Ok(resolved) => resolved,
        Err(error) => {
            blockers.push(text::escape(&error.to_string()));
            return Ok(ResourcePlan {
                memory_bytes: 0,
                millicpus: 0,
                disk_bytes: 0,
                build_memory_bytes: 0,
                clamped: false,
            });
        }
    };

    let builds = model.default_services().any(|s| s.has_build);
    if builds && resolved.build_memory.bytes() < MIN_BUILD_MEMORY_BYTES {
        blockers.push(format!(
            "this project builds images, and the build would get {}. The engine spike saw 2G fail every time and 3G succeed, so raise the memory in the app's Settings or in {}.",
            resolved.build_memory,
            brand::CONFIG_FILE
        ));
    }

    resource_plan(&resolved, project)
}

/// What this machine has now, and what it keeps for itself.
pub(crate) fn host_now(service: &Service, project: &Project, settings: &UserSettings) -> HostResources {
    let info = service.deps().host_info(&project.root);
    HostResources {
        available_memory: info.memory_available.or(info.memory_total).unwrap_or(0),
        memory_reserve: settings.memory_reserve_bytes,
        available_millicpus: info.cpus.unwrap_or(0).saturating_mul(1000),
        cpu_reserve_millis: settings.cpu_reserve_millis,
        available_disk: info.disk_free.unwrap_or(0),
        disk_reserve: settings.disk_reserve_bytes,
    }
}

/// Works out limits from the project's request and a person's choice.
///
/// The maximums in [`UserSettings`] are there to stop a repository taking over
/// the machine. A person's own choice is not a repository's request, so it may
/// pass them, and only what the host must keep for itself still refuses it. The
/// outer error is a failure to read; the inner one is a machine that cannot
/// spare the limits.
pub(crate) fn resolve_with(
    service: &Service,
    project: &Project,
    settings: &UserSettings,
    chosen: &proto::ResourceChoice,
) -> Result<Result<ResolvedResources, ResourceError>, ApiError> {
    let mut request = resources::load(&project.root).map_err(|e| {
        bad(format!(
            "cannot read {}: {}",
            brand::CONFIG_FILE,
            text::escape(&e.to_string())
        ))
    })?;
    let mut limits = settings.limits().map_err(internal)?;
    if let Some(memory) = chosen.memory_bytes.and_then(ByteSize::new) {
        request.memory = Some(memory);
        request.build_memory = None;
        limits.max_memory = limits.max_memory.max(memory);
        limits.max_build_memory = limits.max_build_memory.map(|most| most.max(memory));
    }
    if let Some(cpus) = chosen.millicpus.and_then(MilliCpus::new) {
        request.cpus = Some(cpus);
        limits.max_cpus = limits.max_cpus.max(cpus);
    }
    if let Some(disk) = chosen.disk_bytes.and_then(ByteSize::new) {
        request.disk = Some(disk);
        limits.max_disk = limits.max_disk.max(disk);
    }
    Ok(resources::resolve(&request, &limits, &host_now(service, project, settings)))
}

/// The plan for these limits. A sandbox's disk can only grow, so a plan never
/// says it will be smaller than the one the sandbox has.
pub(crate) fn resource_plan(resolved: &ResolvedResources, project: &Project) -> Result<ResourcePlan, ApiError> {
    let existing = SandboxRecord::load(&project.dir).map_err(internal)?;
    Ok(ResourcePlan {
        memory_bytes: resolved.memory.bytes(),
        millicpus: resolved.cpus.millis(),
        disk_bytes: resolved.disk.bytes().max(existing.map_or(0, |r| r.disk_bytes)),
        build_memory_bytes: resolved.build_memory.bytes(),
        clamped: !resolved.clamped.is_empty(),
    })
}

fn plan_secrets(project: &Project, blockers: &mut Vec<String>) -> Result<Vec<SecretPlan>, ApiError> {
    let Some(meta) = hoststate::read_meta(&project.dir).map_err(internal)? else {
        return Ok(Vec::new());
    };
    let mut plans = Vec::new();
    for assignment in meta.assignments.iter() {
        let (route, bound_host) = match assignment.delivery() {
            Delivery::Proxy { bound_host } => {
                (SecretRoute::Proxy, Some(bound_host.as_str().to_owned()))
            }
            Delivery::Injected => (SecretRoute::Injected, None),
            Delivery::InjectedFile => (SecretRoute::InjectedFile, None),
            Delivery::BuildSecret => (SecretRoute::BuildSecret, None),
        };
        plans.push(SecretPlan {
            name: text::escape(assignment.name().as_str()),
            route,
            services: assignment
                .services()
                .iter()
                .map(|s| text::escape(s.as_str()))
                .collect(),
            bound_host,
        });
    }
    if plans.iter().any(|p| p.route == SecretRoute::Proxy) {
        blockers.push(
            "a secret is set to be held by the proxy, which needs the gateway to terminate TLS. The engine spike found that breaks HTTPS inside nested containers and build steps, and the choice has not been settled, so this cannot be started yet. Assign the secret to a service instead."
                .to_owned(),
        );
    }
    Ok(plans)
}

fn plan_network(
    project: &Project,
    secrets: &[SecretPlan],
    blockers: &mut Vec<String>,
) -> Result<NetworkPlan, ApiError> {
    let approvals = Approvals::load(&project.dir).map_err(internal)?;
    let needs_intercept = secrets.iter().any(|s| s.route == SecretRoute::Proxy);
    let run_hosts: Vec<String> = approvals.run_hosts.iter().map(|h| text::escape(h)).collect();
    for host in &run_hosts {
        if host.contains('*') {
            blockers.push(format!("an approved host is a pattern, not a name: {host}"));
        }
    }
    Ok(NetworkPlan {
        build_hosts: crate::buildhosts::hosts_for(&project.root),
        run_hosts,
        // Never the intercepting mode while the question is open: it is the
        // mode that breaks nested HTTPS.
        tls: if needs_intercept {
            TlsPlan::Intercept
        } else {
            TlsPlan::ServerNameOnly
        },
        merged_for_whole_life: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use projectcheck::compose::{Port, Replicas};

    fn service(name: &str, ports: Vec<Port>) -> ComposeService {
        ComposeService {
            name: name.to_owned(),
            image: None,
            has_build: false,
            replicas: Replicas {
                count: Some(1),
                expression: None,
                dynamic: false,
            },
            profiles: Vec::new(),
            ports,
            mounts: Vec::new(),
            env_keys: Vec::new(),
        }
    }

    fn port(published: Option<&str>, target: Option<u16>) -> Port {
        Port {
            target,
            target_end: None,
            published: published.map(str::to_owned),
            interface: Interface::All,
            protocol: "tcp".to_owned(),
            dynamic: false,
        }
    }

    #[test]
    fn a_fixed_port_is_planned_and_marked_as_published_everywhere() {
        let web = service("web", vec![port(Some("3000"), Some(3000))]);
        let mut blockers = Vec::new();
        let ports = plan_ports(&[&web], &mut blockers);
        assert!(blockers.is_empty());
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].host_port, 3000);
        assert_eq!(ports[0].container_port, 3000);
        assert!(ports[0].was_all_interfaces);
    }

    #[test]
    fn a_port_nobody_can_name_blocks_the_plan() {
        let a = service("a", vec![port(None, Some(80))]);
        let b = service("b", vec![port(Some("8000-8005"), Some(80))]);
        let mut blockers = Vec::new();
        let ports = plan_ports(&[&a, &b], &mut blockers);
        assert!(ports.is_empty());
        assert_eq!(blockers.len(), 2);
    }

    #[test]
    fn a_port_the_engine_cannot_forward_as_tcp_blocks_the_plan() {
        let mut udp = port(Some("5353"), Some(53));
        udp.protocol = "udp".to_owned();
        let mut range = port(Some("8000"), Some(80));
        range.target_end = Some(81);
        let a = service("a", vec![udp, port(Some("3000"), Some(3000))]);
        let b = service("b", vec![range]);
        let mut blockers = Vec::new();
        let ports = plan_ports(&[&a, &b], &mut blockers);
        assert_eq!(ports.iter().map(|p| p.host_port).collect::<Vec<_>>(), vec![3000]);
        assert_eq!(blockers.len(), 2, "{blockers:?}");
        assert!(blockers[0].contains("5353/udp"), "{blockers:?}");
        assert!(blockers[1].contains("range"), "{blockers:?}");
    }

    #[test]
    fn two_services_wanting_one_host_port_block_the_plan() {
        let a = service("a", vec![port(Some("3000"), Some(3000))]);
        let b = service("b", vec![port(Some("3000"), Some(80))]);
        let mut blockers = Vec::new();
        let ports = plan_ports(&[&a, &b], &mut blockers);
        assert_eq!(ports.len(), 1);
        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].contains("3000"));
    }

    #[test]
    fn the_menu_still_holds_every_host_the_spike_measured() {
        let all = every_build_host();
        assert_eq!(all.len(), 12);
        assert!(all.contains(&"fonts.gstatic.com"));
        assert!(all.contains(&"registry.npmjs.org"));
        // Nothing that would let any name resolve.
        assert!(!all.iter().any(|h| h.contains('*')));
    }
}
