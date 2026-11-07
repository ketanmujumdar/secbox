//! Listing a project's secret names, and storing one value, for a client such
//! as the desktop window that has no keychain access of its own.
//!
//! The daemon already reads the keychain to deliver secrets, so writing here
//! keeps every entry owned by one program, and the window gains no keychain
//! access. Nothing a client sends can widen what it may do: a process that can
//! reach this owner-only socket can already run `secrets set` itself.
//!
//! **Write-only.** No request reads a value back, so a client that is tricked
//! into sending requests can replace a secret but never learn one.
//!
//! **Names checked, never echoed when wrong.** A name that is not valid may be
//! a value pasted into the wrong field, so a refusal says what was wrong
//! without repeating it.
//!
//! **Services from the project only.** A secret can be given only to a service
//! in the project's own Compose file, as the daemon reads it.

use proto::{
    text, ApiError, SecretEntry, SecretsReport, SetSecretArgs, SetSecretReport, MAX_SECRET_BYTES,
};
use secrets::{
    Assignment, AssignmentSet, AuditAction, Delivery, SecretName, SecretValue, ServiceName,
};

use crate::{audit, bad, internal, plan, run, Project, Service};

/// The names, where each goes, and the services a secret could be given to.
pub fn list(project: &Project) -> Result<SecretsReport, ApiError> {
    let meta = hoststate::read_meta(&project.dir).map_err(internal)?;
    let mut secrets = Vec::new();
    if let Some(meta) = &meta {
        for record in &meta.secrets {
            let assignment = meta.assignments.iter().find(|a| a.name() == &record.name);
            secrets.push(SecretEntry {
                name: text::escape(record.name.as_str()),
                stored: true,
                services: assignment
                    .map(|a| a.services().iter().map(|s| text::escape(s.as_str())).collect())
                    .unwrap_or_default(),
                route: assignment.map(|a| route(a.delivery()).to_owned()),
            });
        }
        for name in &meta.pending {
            secrets.push(SecretEntry {
                name: text::escape(name.as_str()),
                stored: false,
                services: Vec::new(),
                route: None,
            });
        }
    }
    secrets.sort_by(|a, b| a.name.cmp(&b.name));
    let (services, services_note) = match plan::service_names(project) {
        Ok(names) => (names.iter().map(|n| text::escape(n)).collect(), None),
        Err(error) => (Vec::new(), Some(error.message)),
    };
    Ok(SecretsReport {
        project_id: project.id.as_str().to_owned(),
        secrets,
        services,
        services_note,
    })
}

pub fn set(
    service: &Service,
    project: &Project,
    args: &SetSecretArgs,
) -> Result<SetSecretReport, ApiError> {
    // A wrong name is not repeated: it may be the value, typed in the wrong box.
    let name = SecretName::new(args.name.as_str()).map_err(|_| {
        bad("that is not a valid secret name. Use letters, digits and underscores, starting with a letter or underscore")
    })?;
    let value = args.value.expose();
    if value.is_empty() {
        return Err(bad("an empty value was not stored"));
    }
    if value.len() > MAX_SECRET_BYTES {
        return Err(bad(format!(
            "the value is longer than the limit of {MAX_SECRET_BYTES} bytes"
        )));
    }
    if value.contains('\0') {
        return Err(bad("the value holds a NUL byte, which cannot be delivered"));
    }

    let known = plan::service_names(project)?;
    let mut services = Vec::new();
    for wanted in &args.services {
        if !known.iter().any(|k| k == wanted) {
            return Err(bad(format!(
                "this project has no service named {}",
                text::escape_short(wanted, 80)
            )));
        }
        let service_name = ServiceName::new(wanted.as_str())
            .map_err(|e| bad(format!("bad service name: {}", text::escape(&e.to_string()))))?;
        if !services.contains(&service_name) {
            services.push(service_name);
        }
    }
    services.sort();

    let mut meta = hoststate::read_meta(&project.dir)
        .map_err(internal)?
        .unwrap_or_else(|| hoststate::ProjectMeta::new(&project.root));
    let replaced = meta.has(&name);

    // The delivery chosen before, by `secrets set --as-file` for example, is
    // kept. A new assignment goes into the environment.
    let delivery = meta
        .assignments
        .iter()
        .find(|a| a.name() == &name)
        .map(|a| a.delivery().clone())
        .unwrap_or(Delivery::Injected);
    let mut all: Vec<Assignment> = meta
        .assignments
        .iter()
        .filter(|a| a.name() != &name)
        .cloned()
        .collect();
    if !services.is_empty() {
        all.push(
            Assignment::new(name.clone(), delivery, services.clone())
                .map_err(|e| bad(text::escape(&e.to_string())))?,
        );
    }
    let assignments = AssignmentSet::new(all).map_err(|e| bad(text::escape(&e.to_string())))?;

    let store = service.deps().store(&project.id);
    store
        .put(&name, &SecretValue::new(value))
        .map_err(|e| internal(format!("the keychain refused: {}", text::escape(&e.to_string()))))?;
    meta.record(&name, hoststate::SecretSource::Prompt);
    meta.assignments = assignments;
    // Written straight after the keychain: a value there must never go unrecorded.
    hoststate::write_meta(&project.dir, &meta).map_err(internal)?;
    hoststate::audit(&project.dir, AuditAction::Stored, &name).map_err(internal)?;
    hoststate::audit(&project.dir, AuditAction::AssignmentChanged, &name).map_err(internal)?;
    service.record(
        audit::entry(service.deps().now_unix(), audit::Action::SecretStored)
            .project(project.id.as_str())
            .subject(name.as_str())
            .detail(if services.is_empty() {
                "stored from a client, given to no service".to_owned()
            } else {
                let list: Vec<&str> = services.iter().map(|s| s.as_str()).collect();
                format!("stored from a client, given to {}", list.join(", "))
            }),
    );

    let running = run::status_of(service, project)
        .map(|state| matches!(state, proto::SandboxState::Running))
        .unwrap_or(false);
    Ok(SetSecretReport {
        name: name.as_str().to_owned(),
        replaced,
        services: services.iter().map(|s| s.as_str().to_owned()).collect(),
        applies_on_next_start: running,
    })
}

fn route(delivery: &Delivery) -> &'static str {
    match delivery {
        Delivery::Injected => "injected into the environment",
        Delivery::InjectedFile => "injected as a file",
        Delivery::BuildSecret => "build secret",
        Delivery::Proxy { .. } => "held by the proxy",
    }
}
