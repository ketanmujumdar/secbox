//! Fixed argument lists for the microsandbox CLI, checked against v0.7.13 on macOS.
//! See `docs/spikes/0001-engine-compose.md`. Each builder validates anything that
//! could change how the CLI parses the list.

use std::path::Path;

use crate::error::{EngineError, Result};
use crate::types::{
    CreateSpec, ExecRequest, GuestPath, LogOptions, NetworkPolicy, SandboxName, TlsMode,
};

fn strings<const N: usize>(items: [&str; N]) -> Vec<String> {
    items.into_iter().map(str::to_owned).collect()
}

fn validate_image(image: &str) -> Result<()> {
    let ok = !image.is_empty()
        && !image.starts_with('-')
        && image
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-/:@".contains(c));
    if ok {
        Ok(())
    } else {
        Err(EngineError::InvalidInput {
            field: "image",
            reason: format!("{image:?} is not a plain image reference"),
        })
    }
}

/// Network rule tokens are comma-separated and use `@` and `:`, so a hostname with
/// those characters could add rules of its own. Only plain DNS names pass.
fn validate_host(host: &str) -> Result<()> {
    let label_ok = |l: &str| {
        !l.is_empty()
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    if host.split('.').count() >= 2 && host.split('.').all(label_ok) {
        Ok(())
    } else {
        Err(EngineError::InvalidInput {
            field: "allowed host",
            reason: format!("{host:?} is not a plain DNS name"),
        })
    }
}

fn validate_env_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    if first_ok && chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Ok(())
    } else {
        Err(EngineError::InvalidInput {
            field: "environment variable",
            reason: format!("{name:?} is not a valid name"),
        })
    }
}

/// The variable that tells the CLI where its state directory is.
pub const HOME_VAR: &str = "MSB_HOME";

pub fn version() -> Vec<String> {
    strings(["--version"])
}

pub fn doctor() -> Vec<String> {
    strings(["doctor"])
}

pub fn status(name: &SandboxName) -> Vec<String> {
    strings(["status", name.as_str(), "--format", "json"])
}

pub fn create(spec: &CreateSpec) -> Result<Vec<String>> {
    validate_image(&spec.image)?;
    let r = spec.resources;
    let mut args = strings(["create", "--quiet", "--name", spec.name.as_str()]);
    args.extend([
        "--memory".to_owned(),
        format!("{}M", r.memory_mib),
        "--cpus".to_owned(),
        r.cpus.to_string(),
        "--root-disk".to_owned(),
        format!("flat:{}G", r.disk_gib),
    ]);
    match &spec.network {
        NetworkPolicy::PublicInternet => {}
        NetworkPolicy::DenyAll => args.push("--no-net".to_owned()),
        NetworkPolicy::AllowHosts { hosts, tls } => {
            args.push("--no-net".to_owned());
            // No allow@dns: without it DNS is filtered to these names. With it the
            // guest can resolve any name, which is a way to send data out.
            for host in hosts {
                validate_host(host)?;
                args.push("--net-rule".to_owned());
                args.push(format!("allow@{host}"));
            }
            args.push(
                match tls {
                    TlsMode::ServerNameOnly => "--net-strict=false",
                    TlsMode::Intercept => "--tls-intercept",
                }
                .to_owned(),
            );
        }
    }
    if !spec.secrets.is_empty()
        && !matches!(
            spec.network,
            NetworkPolicy::AllowHosts {
                tls: TlsMode::Intercept,
                ..
            }
        )
    {
        return Err(EngineError::NotSupported {
            operation: "secrets without TLS interception",
            reason: "injection was only verified with an allowlist and TLS interception",
        });
    }
    for secret in &spec.secrets {
        validate_env_name(&secret.env)?;
        if secret.hosts.is_empty() {
            return Err(EngineError::InvalidInput {
                field: "secret",
                reason: format!("{} has no allowed hosts", secret.env),
            });
        }
        for host in &secret.hosts {
            validate_host(host)?;
        }
        args.push("--secret".to_owned());
        args.push(format!("{}@{}", secret.env, secret.hosts.join(",")));
    }
    for port in &spec.ports {
        if port.0 == 0 {
            return Err(EngineError::InvalidInput {
                field: "port",
                reason: "must not be zero".to_owned(),
            });
        }
        args.push("--port".to_owned());
        args.push(format!("127.0.0.1:{0}:{0}", port.0));
        // --no-net denies inbound traffic too, which resets connections to a
        // published port. Open only that port, and only from the host's loopback.
        if !matches!(spec.network, NetworkPolicy::PublicInternet) {
            args.push("--net-rule".to_owned());
            args.push(format!("allow:ingress@127.0.0.1/32:tcp:{}", port.0));
        }
    }
    args.push(spec.image.clone());
    Ok(args)
}

// No --quiet on the next four: on v0.7.13 it hides the error text of `start` and
// `remove`, and the adapter needs that text to report failures.
pub fn start(name: &SandboxName) -> Vec<String> {
    strings(["start", name.as_str()])
}

pub fn stop(name: &SandboxName) -> Vec<String> {
    strings(["stop", name.as_str()])
}

pub fn destroy(name: &SandboxName) -> Vec<String> {
    strings(["remove", "--force", name.as_str()])
}

fn exec_options(args: &mut Vec<String>, request: &ExecRequest) -> Result<()> {
    if request.command.is_empty() {
        return Err(EngineError::InvalidInput {
            field: "command",
            reason: "must not be empty".to_owned(),
        });
    }
    for (key, value) in &request.env {
        validate_env_name(key)?;
        args.push("--env".to_owned());
        args.push(format!("{key}={value}"));
    }
    if let Some(dir) = &request.workdir {
        args.push("--workdir".to_owned());
        args.push(dir.as_str().to_owned());
    }
    Ok(())
}

pub fn exec(name: &SandboxName, request: &ExecRequest) -> Result<Vec<String>> {
    let mut args = strings(["exec", "--no-stdin", "--no-tty"]);
    exec_options(&mut args, request)?;
    if let Some(timeout) = request.timeout {
        args.push("--timeout".to_owned());
        args.push(format!("{}s", timeout.as_secs().max(1)));
    }
    args.push(name.as_str().to_owned());
    args.push("--".to_owned());
    args.extend(request.command.iter().cloned());
    Ok(args)
}

/// Like [`exec`], for output wanted as it arrives. Measured on v0.7.13, plain
/// `--no-tty` holds all output back until the command ends, and `--stream` does
/// not. It takes the standard input the CLI was given, so the caller gives it
/// none. Output stays separated, and exit codes and `--timeout` work the same.
pub fn exec_stream(name: &SandboxName, request: &ExecRequest) -> Result<Vec<String>> {
    let mut args = strings(["exec", "--stream", "--no-tty"]);
    exec_options(&mut args, request)?;
    if let Some(timeout) = request.timeout {
        args.push("--timeout".to_owned());
        args.push(format!("{}s", timeout.as_secs().max(1)));
    }
    args.push(name.as_str().to_owned());
    args.push("--".to_owned());
    args.extend(request.command.iter().cloned());
    Ok(args)
}

/// Runs a command on the caller's own terminal or pipes, so unlike [`exec`] it
/// leaves standard input connected. The CLI's `exec` boots a stopped sandbox,
/// so the caller must have checked that this one is running. `--tty` conflicts
/// with `--stream`, so a real terminal gets one and pipes get the other.
pub fn exec_terminal(name: &SandboxName, request: &ExecRequest, tty: bool) -> Result<Vec<String>> {
    let mut args = if tty {
        strings(["exec", "--tty"])
    } else {
        strings(["exec", "--stream", "--no-tty"])
    };
    exec_options(&mut args, request)?;
    args.push(name.as_str().to_owned());
    args.push("--".to_owned());
    args.extend(request.command.iter().cloned());
    Ok(args)
}

/// Starts a job that keeps running, such as `dockerd`. The CLI prints its job ID.
pub fn exec_detached(name: &SandboxName, request: &ExecRequest) -> Result<Vec<String>> {
    let mut args = strings(["exec", "--detach", "--no-stdin"]);
    exec_options(&mut args, request)?;
    args.push(name.as_str().to_owned());
    args.push("--".to_owned());
    args.extend(request.command.iter().cloned());
    Ok(args)
}

/// The CLI reads `SANDBOX:/path` as a sandbox path, so a host path containing a
/// colon is refused instead of guessing how it would be parsed.
pub fn copy_in(name: &SandboxName, host: &Path, guest: &GuestPath) -> Result<Vec<String>> {
    let invalid = |reason: &str| EngineError::InvalidInput {
        field: "host path",
        reason: reason.to_owned(),
    };
    let host = host
        .to_str()
        .ok_or_else(|| invalid("must be valid UTF-8"))?;
    if !host.starts_with('/') {
        return Err(invalid("must be absolute"));
    }
    if host.contains(':') {
        return Err(invalid("must not contain a colon"));
    }
    Ok(vec![
        "copy".to_owned(),
        host.to_owned(),
        format!("{name}:{}", guest.as_str()),
    ])
}

pub fn logs(name: &SandboxName, options: &LogOptions) -> Vec<String> {
    let mut args = strings(["logs"]);
    if let Some(tail) = options.tail {
        args.push("--tail".to_owned());
        args.push(tail.to_string());
    }
    args.push(name.as_str().to_owned());
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{HostPort, Resources, SecretRule};
    use std::time::Duration;

    fn name() -> SandboxName {
        SandboxName::new("demo").unwrap()
    }

    fn spec() -> CreateSpec {
        CreateSpec {
            name: name(),
            image: "docker:dind".to_owned(),
            resources: Resources::new(6144, 4, 30).unwrap(),
            network: NetworkPolicy::DenyAll,
            secrets: vec![],
            ports: vec![HostPort(3000)],
        }
    }

    #[test]
    fn a_terminal_command_keeps_stdin_and_chooses_its_tty() {
        let mut request = ExecRequest::new(["sh"]);
        request.workdir = Some(GuestPath::new("/work").unwrap());
        assert_eq!(
            exec_terminal(&name(), &request, true).unwrap(),
            ["exec", "--tty", "--workdir", "/work", "demo", "--", "sh"]
        );
        assert_eq!(
            exec_terminal(&name(), &request, false).unwrap(),
            ["exec", "--stream", "--no-tty", "--workdir", "/work", "demo", "--", "sh"]
        );
        assert!(exec_terminal(&name(), &ExecRequest::new(Vec::<String>::new()), true).is_err());
    }

    #[test]
    fn a_stream_asks_for_output_as_it_arrives() {
        let mut request = ExecRequest::new(["docker", "compose", "logs"]);
        request.workdir = Some(GuestPath::new("/work").unwrap());
        assert_eq!(
            exec_stream(&name(), &request).unwrap(),
            [
                "exec", "--stream", "--no-tty", "--workdir", "/work", "demo", "--", "docker",
                "compose", "logs"
            ]
        );
        request.timeout = Some(Duration::from_secs(60));
        let args = exec_stream(&name(), &request).unwrap();
        assert_eq!(args[5..7], ["--timeout", "60s"]);
        assert!(exec_stream(&name(), &ExecRequest::new(Vec::<String>::new())).is_err());
    }

    #[test]
    fn verified_commands() {
        assert_eq!(version(), ["--version"]);
        assert_eq!(doctor(), ["doctor"]);
        assert_eq!(status(&name()), ["status", "demo", "--format", "json"]);
    }

    #[test]
    fn create_full_argv() {
        assert_eq!(
            create(&spec()).unwrap(),
            [
                "create",
                "--quiet",
                "--name",
                "demo",
                "--memory",
                "6144M",
                "--cpus",
                "4",
                "--root-disk",
                "flat:30G",
                "--no-net",
                "--port",
                "127.0.0.1:3000:3000",
                "--net-rule",
                "allow:ingress@127.0.0.1/32:tcp:3000",
                "docker:dind",
            ]
        );
    }

    #[test]
    fn create_allowlist_adds_rules_and_no_dns_rule() {
        let mut s = spec();
        s.network = NetworkPolicy::AllowHosts {
            hosts: vec!["registry.npmjs.org".into(), "a.b".into()],
            tls: TlsMode::ServerNameOnly,
        };
        let args = create(&s).unwrap();
        for wanted in [
            "--no-net",
            "allow@registry.npmjs.org",
            "allow@a.b",
            "--net-strict=false",
        ] {
            assert!(args.iter().any(|a| a == wanted), "{wanted}");
        }
        assert!(!args.iter().any(|a| a == "allow@dns"));
    }

    #[test]
    fn a_published_port_is_opened_only_from_loopback_under_a_closed_network() {
        let mut s = spec();
        s.ports = vec![HostPort(3000)];
        for network in [
            NetworkPolicy::DenyAll,
            NetworkPolicy::AllowHosts {
                hosts: vec!["a.b".into()],
                tls: TlsMode::ServerNameOnly,
            },
        ] {
            s.network = network;
            let args = create(&s).unwrap();
            assert!(args.iter().any(|a| a == "allow:ingress@127.0.0.1/32:tcp:3000"));
            assert!(!args.iter().any(|a| a.contains("ingress@private")));
        }
    }

    #[test]
    fn create_intercept_mode_uses_tls_flag() {
        let mut s = spec();
        s.network = NetworkPolicy::AllowHosts {
            hosts: vec!["a.b".into()],
            tls: TlsMode::Intercept,
        };
        let args = create(&s).unwrap();
        assert!(args.iter().any(|a| a == "--tls-intercept"));
        assert!(!args.iter().any(|a| a == "--net-strict=false"));
    }

    #[test]
    fn create_public_network_adds_no_network_flag() {
        let mut s = spec();
        s.network = NetworkPolicy::PublicInternet;
        let args = create(&s).unwrap();
        for flag in [
            "--no-net",
            "--net-rule",
            "--net-strict=false",
            "--tls-intercept",
        ] {
            assert!(!args.iter().any(|a| a == flag), "{flag}");
        }
    }

    #[test]
    fn secret_argv_names_the_variable_and_hosts_only() {
        let mut s = spec();
        s.network = NetworkPolicy::AllowHosts {
            hosts: vec!["api.example.com".into()],
            tls: TlsMode::Intercept,
        };
        s.secrets = vec![SecretRule {
            env: "API_KEY".into(),
            hosts: vec!["api.example.com".into(), "b.example.com".into()],
        }];
        let args = create(&s).unwrap();
        assert!(args
            .iter()
            .any(|a| a == "API_KEY@api.example.com,b.example.com"));
    }

    #[test]
    fn secret_needs_interception() {
        let mut s = spec();
        s.secrets = vec![SecretRule {
            env: "API_KEY".into(),
            hosts: vec!["a.b".into()],
        }];
        assert!(matches!(create(&s), Err(EngineError::NotSupported { .. })));
    }

    #[test]
    fn secret_rejects_injection_and_empty_hosts() {
        for (env, hosts) in [
            ("A=b", vec!["a.b"]),
            ("1A", vec!["a.b"]),
            ("", vec!["a.b"]),
            ("OK", vec![]),
            ("OK", vec!["a.b,evil.c"]),
            ("OK", vec!["a.b:query"]),
        ] {
            let mut s = spec();
            s.network = NetworkPolicy::AllowHosts {
                hosts: vec!["a.b".into()],
                tls: TlsMode::Intercept,
            };
            s.secrets = vec![SecretRule {
                env: env.into(),
                hosts: hosts.into_iter().map(str::to_owned).collect(),
            }];
            assert!(
                matches!(create(&s), Err(EngineError::InvalidInput { .. })),
                "{env:?}"
            );
        }
    }

    #[test]
    fn create_never_binds_beyond_loopback() {
        for arg in create(&spec()).unwrap() {
            assert!(!arg.starts_with("0.0.0.0"), "{arg}");
        }
    }

    #[test]
    fn create_rejects_rule_injection() {
        for host in [
            "a.com,allow@public",
            "public",
            "*.com",
            "a.com:tcp:443",
            "a@b.com",
            "-a.com",
            "",
        ] {
            let mut s = spec();
            s.network = NetworkPolicy::AllowHosts {
                hosts: vec![host.to_owned()],
                tls: TlsMode::ServerNameOnly,
            };
            assert!(create(&s).is_err(), "{host:?}");
        }
    }

    #[test]
    fn create_rejects_option_like_image() {
        for image in ["--privileged", "", "a b", "a;b"] {
            let mut s = spec();
            s.image = image.to_owned();
            assert!(create(&s).is_err(), "{image:?}");
        }
    }

    #[test]
    fn create_rejects_port_zero() {
        let mut s = spec();
        s.ports = vec![HostPort(0)];
        assert!(create(&s).is_err());
    }

    #[test]
    fn lifecycle_argv() {
        assert_eq!(start(&name()), ["start", "demo"]);
        assert_eq!(stop(&name()), ["stop", "demo"]);
        assert_eq!(destroy(&name()), ["remove", "--force", "demo"]);
    }

    #[test]
    fn exec_keeps_arguments_intact_after_separator() {
        let req = ExecRequest::new(["sh", "-c", "echo $HOME; rm -rf /"]);
        assert_eq!(
            exec(&name(), &req).unwrap(),
            [
                "exec",
                "--no-stdin",
                "--no-tty",
                "demo",
                "--",
                "sh",
                "-c",
                "echo $HOME; rm -rf /",
            ]
        );
    }

    #[test]
    fn exec_passes_timeout_and_rejects_empty() {
        let mut req = ExecRequest::new(["true"]);
        req.timeout = Some(Duration::from_secs(90));
        let args = exec(&name(), &req).unwrap();
        assert_eq!(args[3..5], ["--timeout".to_owned(), "90s".to_owned()]);
        assert!(exec(&name(), &ExecRequest::new(Vec::<String>::new())).is_err());
    }

    #[test]
    fn exec_env_and_workdir() {
        let mut req = ExecRequest::new(["true"]);
        req.env = vec![("A".into(), "b c".into())];
        req.workdir = Some(GuestPath::new("/work").unwrap());
        assert_eq!(
            exec(&name(), &req).unwrap(),
            [
                "exec",
                "--no-stdin",
                "--no-tty",
                "--env",
                "A=b c",
                "--workdir",
                "/work",
                "demo",
                "--",
                "true",
            ]
        );
        req.env = vec![("A=b".into(), "c".into())];
        assert!(exec(&name(), &req).is_err());
    }

    #[test]
    fn exec_detached_argv() {
        let req = ExecRequest::new(["dockerd"]);
        assert_eq!(
            exec_detached(&name(), &req).unwrap(),
            ["exec", "--detach", "--no-stdin", "demo", "--", "dockerd"]
        );
    }

    #[test]
    fn copy_in_argv_and_path_rules() {
        let guest = GuestPath::new("/work").unwrap();
        assert_eq!(
            copy_in(&name(), Path::new("/tmp/x"), &guest).unwrap(),
            ["copy", "/tmp/x", "demo:/work"]
        );
        assert!(copy_in(&name(), Path::new("rel"), &guest).is_err());
        assert!(copy_in(&name(), Path::new("/tmp/a:b"), &guest).is_err());
    }

    #[test]
    fn logs_argv() {
        assert_eq!(logs(&name(), &LogOptions::default()), ["logs", "demo"]);
        assert_eq!(
            logs(&name(), &LogOptions { tail: Some(5) }),
            ["logs", "--tail", "5", "demo"]
        );
    }
}
