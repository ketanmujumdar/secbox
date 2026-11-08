//! Making approved ports reachable through the engine's forward.
//!
//! The engine listens on the host's `127.0.0.1` and forwards each approved
//! port to the guest's own network address, not to the guest's loopback. A
//! project that publishes `127.0.0.1:8080:80`, as a careful project should,
//! then listens only on the guest's loopback, and the forward finds nothing:
//! the host accepts the connection and no answer ever comes.
//!
//! So on every start the host writes a small Compose override that replaces
//! the `ports` of each service that publishes any. An approved port is
//! published on every interface inside the guest, with the number the person
//! approved on both sides of Docker's mapping. A port that was not approved is
//! not published inside the guest at all.
//!
//! # Why every interface inside the guest is safe
//!
//! The guest's network belongs to that one VM. Nothing reaches its interface
//! but the engine's own gateway:
//!
//! - The engine's listener for a forwarded port is created on the host's
//!   `127.0.0.1` only (`engine::argv`, `127.0.0.1:P:P`), so other devices on
//!   the network cannot reach it. This file does not change that.
//! - Without the open internet, the sandbox is created with `--no-net`, which
//!   denies inbound traffic too, and each approved port gets one rule letting
//!   it in from the host's loopback and nowhere else.
//! - The guest sits on its own small subnet behind the engine's gateway, which
//!   is not routed from the host's network, and another sandbox reaching it
//!   would be going out through its own egress rules, which allow named hosts
//!   only.
//! - The containers in the guest could already reach each other, and the
//!   guest's own address, before this change.
//!
//! And it is narrower than before for ports nobody approved: those used to be
//! published inside the guest as the project wrote them, often on every
//! interface, and are now not published at all.
//!
//! The file holds port numbers and service names only, both checked here, so
//! no text from the project reaches it any other way.

use engine::ExecRequest;
use proto::{text, ApiError, PortPlan};

use crate::settings::SandboxRecord;
use crate::{internal, secret, Project, Service};

/// The override, beside the secret one. It holds no secret, so it does not
/// need the `tmpfs`, but it is written fresh on every start either way.
pub fn override_path() -> String {
    format!("{}/ports.yaml", secret::dir())
}

/// The override for a plan whose `approved` ports are forwarded, or `None`
/// when no service publishes anything and there is nothing to change.
///
/// Every service with a port in the plan gets its whole `ports` list
/// replaced. A plan that can be started has every published port of its
/// default services in it, because anything else is a blocker.
pub fn override_yaml(ports: &[PortPlan], approved: &[u16]) -> Result<Option<String>, ApiError> {
    let mut services: Vec<&str> = ports.iter().map(|p| p.service.as_str()).collect();
    services.sort_unstable();
    services.dedup();
    if services.is_empty() {
        return Ok(None);
    }

    let mut yaml = String::from(
        "# Ports, rewritten by the host on every start. Approved ports are\n\
         # published on every interface inside the sandbox, where the engine's\n\
         # forward from the host's 127.0.0.1 arrives. Others are not published.\n\
         services:\n",
    );
    for service in services {
        if !plain_service_name(service) {
            return Err(internal(format!(
                "cannot publish the ports of service {}: its name is not one Compose accepts",
                text::escape_short(service, 60)
            )));
        }
        let open: Vec<&PortPlan> = ports
            .iter()
            .filter(|p| p.service == service && approved.contains(&p.host_port))
            .collect();
        yaml.push_str(&format!("  \"{service}\":\n"));
        if open.is_empty() {
            yaml.push_str("    ports: !reset []\n");
            continue;
        }
        yaml.push_str("    ports: !override\n");
        for port in open {
            yaml.push_str(&format!(
                "      - target: {}\n        published: \"{}\"\n        protocol: tcp\n",
                port.container_port, port.host_port
            ));
        }
    }
    Ok(Some(yaml))
}

/// Compose's own rule for a service name. Anything else is refused rather than
/// quoted, because it would not have come from a file Compose accepted.
fn plain_service_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        && name.len() <= 255
}

/// Writes the override into the guest and returns its path, or `None` when
/// the plan publishes nothing.
///
/// The approved ports are read from the sandbox's record, which was written
/// when the sandbox was created with exactly those ports forwarded.
pub fn write_override(
    service: &Service,
    project: &Project,
    ports: &[PortPlan],
) -> Result<Option<String>, ApiError> {
    let approved = SandboxRecord::load(&project.dir)
        .map_err(internal)?
        .map(|record| record.ports)
        .unwrap_or_default();
    let Some(yaml) = override_yaml(ports, &approved)? else {
        return Ok(None);
    };
    let path = override_path();
    // The path is an argument, not part of the script, so it needs no quoting.
    let request = ExecRequest::new([
        "sh",
        "-c",
        "umask 077; mkdir -p \"${0%/*}\"; exec cat > \"$0\"",
        &path,
    ]);
    let out = service
        .deps()
        .engine()
        .exec_with_input(&project.sandbox, &request, yaml.as_bytes())
        .map_err(|e| crate::engine_error(e.to_string()))?;
    if out.code != 0 {
        return Err(internal(format!(
            "could not write {} inside the sandbox",
            text::escape(&path)
        )));
    }
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(service: &str, host_port: u16, container_port: u16) -> PortPlan {
        PortPlan {
            service: service.to_owned(),
            host_port,
            container_port,
            mac_port: 0,
            chosen: None,
            was_all_interfaces: false,
            dynamic: false,
        }
    }

    #[test]
    fn nothing_is_written_when_nothing_is_published() {
        assert_eq!(override_yaml(&[], &[]).unwrap(), None);
        assert_eq!(override_yaml(&[], &[8080]).unwrap(), None);
    }

    #[test]
    fn a_loopback_only_port_that_was_approved_is_published_on_every_guest_interface() {
        // `127.0.0.1:8080:80` in the project's file.
        let yaml = override_yaml(&[port("web", 8080, 80)], &[8080]).unwrap().unwrap();
        assert!(yaml.contains("\"web\":\n    ports: !override\n"), "{yaml}");
        assert!(yaml.contains("- target: 80\n        published: \"8080\"\n        protocol: tcp\n"), "{yaml}");
        // No address, so Docker publishes on every interface in the guest,
        // where the engine's forward arrives.
        assert!(!yaml.contains("host_ip"), "{yaml}");
        assert!(!yaml.contains("127.0.0.1:8080"), "{yaml}");
    }

    #[test]
    fn a_port_that_was_not_approved_is_not_published_inside_the_guest() {
        let plan = [port("web", 3000, 3000), port("api", 8080, 8080), port("api", 9000, 9000)];
        let yaml = override_yaml(&plan, &[3000, 9000]).unwrap().unwrap();
        assert!(yaml.contains("published: \"3000\""), "{yaml}");
        assert!(yaml.contains("published: \"9000\""), "{yaml}");
        assert!(!yaml.contains("8080"), "{yaml}");

        let none = override_yaml(&plan, &[]).unwrap().unwrap();
        assert!(none.contains("\"api\":\n    ports: !reset []\n"), "{none}");
        assert!(none.contains("\"web\":\n    ports: !reset []\n"), "{none}");
        assert!(!none.contains("published:"), "{none}");
    }

    #[test]
    fn an_approved_number_outside_the_plan_publishes_nothing() {
        let yaml = override_yaml(&[port("web", 3000, 80)], &[4000]).unwrap().unwrap();
        assert!(!yaml.contains("4000"), "{yaml}");
        assert!(yaml.contains("ports: !reset []"), "{yaml}");
    }

    #[test]
    fn the_container_port_is_kept_and_the_host_number_is_the_approved_one() {
        let yaml = override_yaml(&[port("web", 3000, 80)], &[3000]).unwrap().unwrap();
        assert_eq!(
            yaml.lines().skip_while(|l| !l.starts_with("services:")).collect::<Vec<_>>(),
            [
                "services:",
                "  \"web\":",
                "    ports: !override",
                "      - target: 80",
                "        published: \"3000\"",
                "        protocol: tcp",
            ]
        );
    }

    #[test]
    fn a_service_name_compose_would_not_accept_is_refused() {
        for bad in ["", "a\"b", "a\nb", "-a", "a b", "a:b", "a\\u{0}"] {
            assert!(override_yaml(&[port(bad, 3000, 80)], &[3000]).is_err(), "{bad:?}");
        }
        assert!(override_yaml(&[port("my_app.v2-x", 3000, 80)], &[3000]).is_ok());
    }

    #[test]
    fn the_override_lives_beside_the_secret_one_on_the_guest() {
        assert!(override_path().starts_with(&secret::dir()));
        assert_ne!(override_path(), secret::override_path());
        assert!(!override_path().contains("override.yaml"));
    }
}
