//! Getting secrets from the host's keychain into a sandbox.
//!
//! Three rules shape all of this.
//!
//! **Never through an argument.** A value handed to the engine as a
//! command-line argument is readable by anyone on the host, from the process
//! list. Values go in over standard input instead.
//!
//! **Never onto a disk that outlives the moment.** Inside the guest they land
//! on a small `tmpfs`, mounted `noexec,nosuid,nodev`, which exists only while
//! the sandbox does. If that mount cannot be made, delivery stops: writing a
//! secret to the sandbox's own disk instead would be the quiet failure this
//! project exists to prevent.
//!
//! **Never into the snapshot.** Nothing is written under the project
//! directory, so no secret can reach a diff, an export or a commit. The
//! generated Compose override lives on the same `tmpfs` and holds only paths.
//!
//! What this does not do is keep a secret from the service it was given to.
//! A program can read its own environment, and anything sharing that
//! container can read it too. Assignment limits which services get a value,
//! not what the receiving service may do with it.

use std::collections::BTreeMap;

use engine::ExecRequest;
use proto::{text, ApiError};
use secrets::{Delivery, ResolvedService, SandboxPlaceholders, SecretName, ServiceName};

use crate::{audit, internal, refused, Project, Service};

/// The directory on the guest's `tmpfs`. Built from the product's own name,
/// which is defined in one place.
pub fn dir() -> String {
    format!("/run/{}", brand::STATE_DIR_NAME)
}

/// The generated Compose override, which holds paths and no values.
pub fn override_path() -> String {
    format!("{}/override.yaml", dir())
}

/// Where Compose puts a file-delivered secret inside a service.
pub const GUEST_SECRETS: &str = "/run/secrets";

/// What delivery produced.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Delivered {
    /// The override to add to the Compose command, when there is one.
    pub override_file: Option<String>,
    /// Secret names by service, for the report. Names only.
    pub by_service: BTreeMap<String, Vec<String>>,
}

impl Delivered {
    pub fn is_empty(&self) -> bool {
        self.override_file.is_none()
    }
}

/// Delivers every assigned secret, and returns the override to use.
///
/// This runs on every start, not only the first: the values live on a
/// `tmpfs`, so a sandbox that was stopped and started again has none left.
pub fn deliver(service: &Service, project: &Project) -> Result<Delivered, ApiError> {
    let Some(meta) = hoststate::read_meta(&project.dir).map_err(internal)? else {
        return Ok(Delivered::default());
    };
    let assignments = meta.assignments;
    if assignments.iter().next().is_none() {
        return Ok(Delivered::default());
    }
    if assignments
        .iter()
        .any(|a| matches!(a.delivery(), Delivery::Proxy { .. }))
    {
        return Err(refused(
            "a secret is held by the proxy, which needs the gateway to terminate TLS. That is not settled, so this cannot be started.",
        ));
    }

    let placeholders = SandboxPlaceholders::generate(&assignments)
        .map_err(|e| internal(format!("cannot prepare secrets: {e}")))?;
    let store = service.deps().store(&project.id);

    // Every service named by any assignment, in a stable order.
    let mut services: Vec<ServiceName> = assignments
        .iter()
        .flat_map(|a| a.services().iter().cloned())
        .collect();
    services.sort();
    services.dedup();

    make_tmpfs(service, project)?;

    let mut delivered = Delivered::default();
    let mut file_secrets: BTreeMap<String, ()> = BTreeMap::new();
    let mut plan = Vec::new();

    for name in &services {
        let resolved = assignments
            .resolve_for_service(name, store.as_ref(), &placeholders)
            .map_err(|e| internal(format!("cannot prepare secrets: {e}")))?;
        let names = record_names(&resolved);
        if names.is_empty() {
            continue;
        }
        delivered.by_service.insert(name.as_str().to_owned(), names);

        if !resolved.injected.is_empty() {
            let body = env_file_body(&resolved)?;
            write_file(service, project, &env_path(name), body.as_bytes())?;
        }
        for secret in &resolved.injected_files {
            let path = secret_path(&secret.name);
            if file_secrets.insert(secret.name.as_str().to_owned(), ()).is_none() {
                write_file(service, project, &path, secret.value.expose().as_bytes())?;
            }
        }
        plan.push((
            name.clone(),
            !resolved.injected.is_empty(),
            resolved
                .injected_files
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>(),
        ));
    }

    if plan.is_empty() {
        return Ok(Delivered::default());
    }

    let yaml = override_yaml(&plan);
    let path = override_path();
    write_file(service, project, &path, yaml.as_bytes())?;
    delivered.override_file = Some(path);

    for (name, secret_names) in &delivered.by_service {
        for secret in secret_names {
            service.record(
                audit::entry(service.deps().now_unix(), audit::Action::SecretDelivered)
                    .project(project.id.as_str())
                    .subject(secret)
                    .detail(format!("to {name}")),
            );
        }
    }
    Ok(delivered)
}

fn record_names(resolved: &ResolvedService) -> Vec<String> {
    let mut names: Vec<String> = resolved
        .injected
        .iter()
        .chain(resolved.injected_files.iter())
        .map(|s| s.name.as_str().to_owned())
        .collect();
    names.sort();
    names
}

fn env_path(service: &ServiceName) -> String {
    format!("{}/env.{}", dir(), service.as_str())
}

fn secret_path(name: &SecretName) -> String {
    format!("{}/secret.{}", dir(), name.as_str())
}

/// Builds the body of an environment file.
///
/// A value holding a newline would start a second line, and so define a
/// variable nobody asked for. Rather than guess at an escaping that Compose
/// may or may not share, such a value is refused and the person is told to
/// deliver it as a file.
fn env_file_body(resolved: &ResolvedService) -> Result<String, ApiError> {
    let mut body = String::new();
    for secret in &resolved.injected {
        let value = secret.value.expose();
        if value.contains('\n') || value.contains('\r') || value.contains('\0') {
            return Err(refused(format!(
                "{} has a line break in it, which cannot go in an environment file. Assign it with --as-file instead.",
                text::escape(secret.name.as_str())
            )));
        }
        body.push_str(secret.name.as_str());
        body.push('=');
        body.push_str(value);
        body.push('\n');
    }
    Ok(body)
}

/// The override Compose is given. It names paths and never holds a value.
fn override_yaml(plan: &[(ServiceName, bool, Vec<SecretName>)]) -> String {
    let mut files: Vec<&SecretName> = plan.iter().flat_map(|(_, _, f)| f.iter()).collect();
    files.sort();
    files.dedup();

    let mut yaml = String::from(
        "# Written by the host for one start, on a tmpfs inside the sandbox.\n\
         # It holds paths only. Nothing here is a secret value.\n\
         services:\n",
    );
    for (service, has_env, file_secrets) in plan {
        yaml.push_str(&format!("  \"{}\":\n", service.as_str()));
        if *has_env {
            yaml.push_str("    env_file:\n");
            yaml.push_str(&format!("      - \"{}\"\n", env_path(service)));
        }
        if !file_secrets.is_empty() {
            yaml.push_str("    secrets:\n");
            for name in file_secrets {
                yaml.push_str(&format!("      - \"{}\"\n", name.as_str()));
            }
        }
    }
    if !files.is_empty() {
        yaml.push_str("secrets:\n");
        for name in files {
            yaml.push_str(&format!("  \"{}\":\n", name.as_str()));
            yaml.push_str(&format!("    file: \"{}\"\n", secret_path(name)));
        }
    }
    yaml
}

/// Mounts the small `tmpfs` the values live on, and checks that it is really
/// there. Carrying on without it would put secrets on the sandbox's disk.
fn make_tmpfs(service: &Service, project: &Project) -> Result<(), ApiError> {
    let at = dir();
    let script = format!(
        "set -e; mkdir -p {at}; chmod 700 {at}; \
         grep -q ' {at} ' /proc/mounts || \
         mount -t tmpfs -o size=1m,mode=0700,noexec,nosuid,nodev tmpfs {at}; \
         grep -q ' {at} tmpfs ' /proc/mounts"
    );
    let out = service
        .deps()
        .engine()
        .exec(&project.sandbox, &ExecRequest::new(["sh", "-c", &script]))
        .map_err(|e| crate::engine_error(e.to_string()))?;
    if out.code == 0 {
        return Ok(());
    }
    Err(refused(
        "the sandbox has nowhere safe to keep a secret: a temporary filesystem could not be mounted, and writing one to its disk instead is not something this will do quietly.",
    ))
}

/// Writes one file inside the guest, through standard input.
///
/// The path is passed as an argument to the shell rather than written into
/// the script, so no name needs quoting, and the value never appears in any
/// argument list at all.
fn write_file(
    service: &Service,
    project: &Project,
    path: &str,
    body: &[u8],
) -> Result<(), ApiError> {
    let request = ExecRequest::new(["sh", "-c", "umask 077; exec cat > \"$0\"", path]);
    let out = service
        .deps()
        .engine()
        .exec_with_input(&project.sandbox, &request, body)
        .map_err(|e| crate::engine_error(e.to_string()))?;
    if out.code == 0 {
        Ok(())
    } else {
        // The sandbox's own words could hold anything, including part of what
        // was being written, so they are not repeated here.
        Err(internal(format!(
            "could not write {} inside the sandbox",
            text::escape(path)
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrets::{DeliveredSecret, ProxyEnv, SecretValue};

    fn resolved(env: &[(&str, &str)], files: &[(&str, &str)]) -> ResolvedService {
        ResolvedService {
            service: ServiceName::new("api").unwrap(),
            proxy: Vec::<ProxyEnv>::new(),
            injected: env
                .iter()
                .map(|(n, v)| DeliveredSecret {
                    name: SecretName::new(*n).unwrap(),
                    value: SecretValue::new(*v),
                })
                .collect(),
            injected_files: files
                .iter()
                .map(|(n, v)| DeliveredSecret {
                    name: SecretName::new(*n).unwrap(),
                    value: SecretValue::new(*v),
                })
                .collect(),
            build_secret_names: Vec::new(),
        }
    }

    #[test]
    fn an_environment_file_is_one_line_for_each_secret() {
        let body = env_file_body(&resolved(&[("A", "1"), ("B", "two")], &[])).unwrap();
        assert_eq!(body, "A=1\nB=two\n");
    }

    /// A value with a newline would define a second variable nobody asked
    /// for, so it is refused rather than escaped by guesswork.
    #[test]
    fn a_value_with_a_line_break_is_refused_for_the_environment() {
        for bad in ["a\nEVIL=1", "a\r\nEVIL=1", "a\0b"] {
            let error = env_file_body(&resolved(&[("A", bad)], &[])).unwrap_err();
            assert_eq!(error.code, proto::ErrorCode::Refused);
            assert!(error.message.contains("--as-file"), "{}", error.message);
        }
    }

    #[test]
    fn the_override_names_paths_and_holds_no_value() {
        let plan = vec![
            (
                ServiceName::new("api").unwrap(),
                true,
                vec![SecretName::new("ENCRYPTION_KEY").unwrap()],
            ),
            (ServiceName::new("worker").unwrap(), true, vec![]),
        ];
        let yaml = override_yaml(&plan);
        assert!(yaml.contains("\"api\":"), "{yaml}");
        assert!(
            yaml.contains(&format!("- \"{}\"", env_path(&ServiceName::new("api").unwrap()))),
            "{yaml}"
        );
        assert!(yaml.contains("- \"ENCRYPTION_KEY\""), "{yaml}");
        assert!(
            yaml.contains(&format!(
                "file: \"{}\"",
                secret_path(&SecretName::new("ENCRYPTION_KEY").unwrap())
            )),
            "{yaml}"
        );
        assert!(yaml.contains("\"worker\":"), "{yaml}");
        // A service with no file secrets gets no secrets block.
        assert_eq!(yaml.matches("    secrets:").count(), 1);
    }

    #[test]
    fn a_service_with_nothing_assigned_gets_no_environment_file() {
        let plan = vec![(ServiceName::new("web").unwrap(), false, vec![])];
        let yaml = override_yaml(&plan);
        assert!(!yaml.contains("env_file"), "{yaml}");
    }

    #[test]
    fn everything_written_sits_outside_the_project() {
        let at = dir();
        assert!(at.starts_with("/run/"));
        assert!(override_path().starts_with(&at));
        for path in [
            override_path(),
            env_path(&ServiceName::new("api").unwrap()),
            secret_path(&SecretName::new("A").unwrap()),
        ] {
            assert!(path.starts_with(&at), "{path}");
            // Nothing may land under the snapshot, or it could reach a
            // commit, a diff or an export.
            assert!(!path.contains("/work"), "{path}");
        }
    }
}
