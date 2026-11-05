use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use projectcheck::compose::{
    check_project, parse_file, parse_str, ComposeReport, Finding, FindingKind, Interface,
    MountKind, Severity, MAX_COMPOSE_BYTES,
};
use projectcheck::Error;
use tempfile::TempDir;

fn report(text: &str) -> ComposeReport {
    parse_str(text).unwrap_or_else(|err| panic!("{err}: {text}"))
}

/// A one-service file with `body` at the service level.
fn service(body: &str) -> ComposeReport {
    let indented: String = body.lines().map(|l| format!("    {l}\n")).collect();
    report(&format!("services:\n  app:\n    image: x\n{indented}"))
}

fn of_kind(report: &ComposeReport, kind: FindingKind) -> Vec<&Finding> {
    report.findings.iter().filter(|f| f.kind == kind).collect()
}

/// Asserts that `body` gives a finding of `kind` at `severity` for the service.
fn assert_flag(body: &str, kind: FindingKind, severity: Severity) {
    let out = service(body);
    let found = of_kind(&out, kind);
    assert!(
        found
            .iter()
            .any(|f| f.severity == severity && f.service.as_deref() == Some("app")),
        "{body}: wanted {kind:?} at {severity:?}, got {:#?}",
        out.findings
    );
}

fn assert_clean(body: &str) {
    let out = service(body);
    assert!(out.findings.is_empty(), "{body}: {:#?}", out.findings);
}

// Service flags

#[test]
fn privileged_forms() {
    use FindingKind::Privileged as P;
    for body in [
        "privileged: true",
        "privileged: \"true\"",
        "privileged: yes",
        "privileged: \"on\"",
        "privileged: ${P:-true}",
        "privileged: ${P}",
        "privileged: $P",
    ] {
        assert_flag(body, P, Severity::Critical);
    }
    assert_flag("privileged: ${P:-false}", P, Severity::Medium);
    assert_clean("privileged: false");
    assert_clean("privileged: null");
}

#[test]
fn privileged_of_an_unknown_shape_is_a_finding() {
    for body in [
        "privileged: [true]",
        "privileged: {a: 1}",
        "privileged: 1",
        "privileged: maybe",
    ] {
        assert_flag(body, FindingKind::Unrecognized, Severity::High);
    }
}

#[test]
fn privileged_through_chained_merge_is_seen() {
    let out = report(
        "x-a: &a {privileged: true}\nx-b: &b {<<: *a}\nservices:\n  app:\n    <<: *b\n    image: x\n",
    );
    assert_eq!(of_kind(&out, FindingKind::Privileged).len(), 1);
    assert!(of_kind(&out, FindingKind::Unrecognized).is_empty());
}

#[test]
fn privileged_through_alias_is_seen() {
    let out = report("x-p: &p true\nservices:\n  app:\n    image: x\n    privileged: *p\n");
    assert_eq!(of_kind(&out, FindingKind::Privileged).len(), 1);
}

#[test]
fn tag_cannot_hide_privileged() {
    let out = service("privileged: !custom true");
    assert!(!of_kind(&out, FindingKind::Privileged).is_empty());
    assert!(!of_kind(&out, FindingKind::Unrecognized).is_empty());
}

#[test]
fn compose_reset_and_override_tags_are_reported() {
    let out = service("ports: !override\n  - \"80:80\"");
    assert_eq!(of_kind(&out, FindingKind::PortAllInterfaces).len(), 1);
    assert!(!of_kind(&out, FindingKind::Unrecognized).is_empty());
}

#[test]
fn hook_privileged_is_seen() {
    assert_flag(
        "post_start:\n  - command: id\n    privileged: true",
        FindingKind::Privileged,
        Severity::Critical,
    );
}

#[test]
fn network_mode_forms() {
    use FindingKind::NetworkMode as N;
    assert_flag("network_mode: host", N, Severity::High);
    assert_flag("network_mode: HOST", N, Severity::High);
    assert_flag("network_mode: \" host \"", N, Severity::High);
    assert_flag("network_mode: ${N:-host}", N, Severity::High);
    assert_flag("network_mode: ${N}", N, Severity::High);
    assert_flag("network_mode: \"service:db\"", N, Severity::Medium);
    assert_flag("network_mode: \"container:abc\"", N, Severity::Medium);
    assert_flag("network_mode: ${N:-bridge}", N, Severity::Medium);
    assert_flag(
        "network_mode: weird",
        FindingKind::Unrecognized,
        Severity::High,
    );
    assert_flag(
        "network_mode: [host]",
        FindingKind::Unrecognized,
        Severity::High,
    );
    assert_flag("network_mode: 5", FindingKind::Unrecognized, Severity::High);
    assert_clean("network_mode: bridge");
    assert_clean("network_mode: none");
}

#[test]
fn host_namespaces() {
    assert_flag("pid: host", FindingKind::PidMode, Severity::High);
    assert_flag(
        "pid: \"service:db\"",
        FindingKind::PidMode,
        Severity::Medium,
    );
    assert_flag("ipc: host", FindingKind::IpcMode, Severity::High);
    assert_flag(
        "ipc: \"service:db\"",
        FindingKind::IpcMode,
        Severity::Medium,
    );
    assert_flag("uts: host", FindingKind::UtsMode, Severity::High);
    assert_flag("userns_mode: host", FindingKind::UsernsMode, Severity::High);
    assert_flag("cgroup: host", FindingKind::CgroupMode, Severity::High);
    assert_flag("uts: ${U}", FindingKind::UtsMode, Severity::High);
    assert_flag(
        "cgroup: ${C:-host}",
        FindingKind::CgroupMode,
        Severity::High,
    );
    assert_flag("pid: odd", FindingKind::Unrecognized, Severity::High);
    assert_flag("cgroup: [host]", FindingKind::Unrecognized, Severity::High);
    assert_clean("ipc: private");
    assert_clean("cgroup: private");
}

#[test]
fn cap_add_forms() {
    use FindingKind::CapAdd as C;
    assert_flag("cap_add: [ALL]", C, Severity::Critical);
    assert_flag("cap_add: [all]", C, Severity::Critical);
    assert_flag("cap_add: [CAP_SYS_ADMIN]", C, Severity::Critical);
    assert_flag("cap_add: [\"${C}\"]", C, Severity::Critical);
    assert_flag("cap_add: [\"${C:-SYS_ADMIN}\"]", C, Severity::Critical);
    assert_flag("cap_add: [NET_ADMIN]", C, Severity::High);
    assert_flag("cap_add: [CHOWN]", C, Severity::Medium);
    // A bare string is the wrong shape, but it is still checked.
    let out = service("cap_add: SYS_ADMIN");
    assert!(!of_kind(&out, FindingKind::Unrecognized).is_empty());
    assert!(!of_kind(&out, C).is_empty());
    assert_flag("cap_add: {a: b}", FindingKind::Unrecognized, Severity::High);
    assert_flag(
        "cap_add: [[ALL]]",
        FindingKind::Unrecognized,
        Severity::High,
    );
    assert_clean("cap_drop: [ALL]");
}

#[test]
fn security_opt_forms() {
    use FindingKind::SecurityOpt as S;
    for option in [
        "seccomp:unconfined",
        "seccomp=unconfined",
        "SECCOMP=UNCONFINED",
        "apparmor:unconfined",
        "apparmor=unconfined",
        "label:disable",
        "label=disable",
        "label:type:spc_t",
        "no-new-privileges:false",
        "systempaths=unconfined",
        "${OPT}",
        "${OPT:-seccomp=unconfined}",
    ] {
        assert_flag(&format!("security_opt: [\"{option}\"]"), S, Severity::High);
    }
    assert_flag("security_opt: [\"seccomp:/x.json\"]", S, Severity::Medium);
    assert_flag(
        "security_opt: [\"writable-cgroups=true\"]",
        FindingKind::Unrecognized,
        Severity::High,
    );
    assert_flag(
        "security_opt: {a: b}",
        FindingKind::Unrecognized,
        Severity::High,
    );
    assert_clean("security_opt: [\"no-new-privileges:true\"]");
}

#[test]
fn devices_and_rules() {
    use FindingKind::Device as D;
    assert_flag("devices: [\"/dev/kvm:/dev/kvm\"]", D, Severity::High);
    assert_flag("device_cgroup_rules: [\"c 1:3 mr\"]", D, Severity::High);
    assert_flag("devices: {a: b}", FindingKind::Unrecognized, Severity::High);
}

#[test]
fn other_isolation_fields() {
    assert_flag(
        "use_api_socket: true",
        FindingKind::DockerSocket,
        Severity::Critical,
    );
    assert_flag("runtime: runsc", FindingKind::Weakening, Severity::High);
    assert_flag(
        "provider: {type: x}",
        FindingKind::Weakening,
        Severity::High,
    );
    assert_flag("sysctls: {a: 1}", FindingKind::Weakening, Severity::Medium);
    assert_flag("cgroup_parent: x", FindingKind::Weakening, Severity::Medium);
    assert_flag(
        "volumes_from: [db]",
        FindingKind::Weakening,
        Severity::Medium,
    );
    assert_flag(
        "extra_hosts: [\"h:host-gateway\"]",
        FindingKind::Weakening,
        Severity::Medium,
    );
    assert_flag(
        "label_file: [/etc/passwd]",
        FindingKind::Weakening,
        Severity::High,
    );
}

#[test]
fn unknown_fields_are_findings() {
    assert_flag(
        "Privileged: true",
        FindingKind::Unrecognized,
        Severity::High,
    );
    assert_flag("net: host", FindingKind::Unrecognized, Severity::High);
    assert_flag("? [a]\n: 1", FindingKind::Unrecognized, Severity::High);
    assert_flag(
        "deploy: {sneaky: 1}",
        FindingKind::Unrecognized,
        Severity::High,
    );
    assert_flag("deploy: 5", FindingKind::Unrecognized, Severity::High);
    assert_clean("x-note: anything");
    let out = report("services: {}\nfoo: 1\n");
    assert!(!of_kind(&out, FindingKind::Unrecognized).is_empty());
    let out = report("services: 5\n");
    assert!(!of_kind(&out, FindingKind::Unrecognized).is_empty());
    let out = report("name: x\n");
    assert!(!of_kind(&out, FindingKind::Unrecognized).is_empty());
    let out = report("services:\n  app: 5\n");
    assert!(!of_kind(&out, FindingKind::Unrecognized).is_empty());
}

// Mounts

#[test]
fn docker_socket_forms() {
    use FindingKind::DockerSocket as D;
    for mount in [
        "/var/run/docker.sock:/var/run/docker.sock",
        "/run/docker.sock:/x",
        "//var//run//docker.sock:/x",
        "/var/run/../run/docker.sock:/x",
        "/VAR/RUN/DOCKER.SOCK:/x",
        "${S:-/var/run/docker.sock}:/x",
        "${S}:/var/run/docker.sock",
        "~/.docker/run/docker.sock:/x",
        "/run/podman/podman.sock:/x",
        "/run/containerd/containerd.sock:/x",
        "/var/run:/x",
        "/run:/x",
        "/:/host",
        "/var/run/foo/../:/x",
    ] {
        assert_flag(&format!("volumes: [\"{mount}\"]"), D, Severity::Critical);
    }
    assert_flag(
        "volumes:\n  - type: bind\n    source: /var/run/docker.sock\n    target: /x",
        D,
        Severity::Critical,
    );
    assert_flag(
        "volumes:\n  - type: npipe\n    source: //./pipe/docker_engine\n    target: /x",
        D,
        Severity::Critical,
    );
}

#[test]
fn a_volume_mounted_at_run_in_the_container_is_not_the_socket() {
    assert_clean("volumes: [\"data:/run\"]");
    assert_clean("volumes: [\"data:/var/run\"]");
}

#[test]
fn outside_bind_mounts_short_syntax() {
    use FindingKind::HostMount as H;
    for mount in [
        "/etc:/x",
        "/etc:/x:ro",
        "~:/x",
        "~/.ssh:/x",
        "~root/.ssh:/x",
        "..:/x",
        "../secret:/x",
        "./a/../../b:/x",
        "./../b:/x",
        "a/../../b:/x",
        "..\\\\b:/x",
        "C:\\\\Users:/x",
        "${HOME}/.ssh:/x",
        "$HOME/.ssh:/x",
        "${SRC}:/x",
        "${SRC:-/etc}:/x",
        "${SRC:-}:/x",
    ] {
        assert_flag(&format!("volumes: [\"{mount}\"]"), H, Severity::High);
    }
}

#[test]
fn outside_bind_mounts_long_syntax() {
    use FindingKind::HostMount as H;
    for source in ["/etc", "~/.ssh", "../x", "./a/../../x", "${HOME}"] {
        assert_flag(
            &format!("volumes:\n  - type: bind\n    source: \"{source}\"\n    target: /x"),
            H,
            Severity::High,
        );
    }
    // Claiming to be a volume does not hide a path.
    assert_flag(
        "volumes:\n  - type: volume\n    source: /etc\n    target: /x",
        H,
        Severity::High,
    );
    // An unknown type still has its source checked.
    let out = service("volumes:\n  - type: strange\n    source: /etc\n    target: /x");
    assert!(!of_kind(&out, H).is_empty());
    assert!(!of_kind(&out, FindingKind::Unrecognized).is_empty());
}

#[test]
fn relative_mounts_are_info_and_record_read_only() {
    let out = service("volumes: [\"./a/b.sql:/x:ro\", \"./c:/y\", \"d/../e:/z:rw\"]");
    let found = of_kind(&out, FindingKind::ProjectMount);
    assert_eq!(found.len(), 3);
    assert!(found.iter().all(|f| f.severity == Severity::Info));
    assert!(found[0].message.contains("read-only"));
    assert!(found[1].message.contains("writable"));
    let mounts = &out.model.service("app").unwrap().mounts;
    assert_eq!(
        mounts.iter().map(|m| m.read_only).collect::<Vec<_>>(),
        vec![true, false, false]
    );
    assert!(mounts.iter().all(|m| m.kind == MountKind::Project));
}

#[test]
fn long_relative_mounts_record_read_only() {
    let out = service(
        "volumes:\n  - {type: bind, source: ./a, target: /x, read_only: true}\n  - {type: bind, source: bare, target: /y}\n",
    );
    let mounts = &out.model.service("app").unwrap().mounts;
    assert_eq!(mounts[0].kind, MountKind::Project);
    assert!(mounts[0].read_only);
    assert_eq!(mounts[1].kind, MountKind::Project);
    assert!(!mounts[1].read_only);
}

#[test]
fn ro_and_rw_together_count_as_writable() {
    let out = service("volumes: [\"./a:/x:ro,rw\"]");
    assert!(!out.model.service("app").unwrap().mounts[0].read_only);
}

#[test]
fn malformed_read_only_counts_as_writable_and_is_reported() {
    let out = service("volumes:\n  - {type: bind, source: ./a, target: /x, read_only: maybe}\n");
    assert!(!out.model.service("app").unwrap().mounts[0].read_only);
    assert!(!of_kind(&out, FindingKind::Unrecognized).is_empty());
}

#[test]
fn variable_in_a_relative_or_named_source_is_reported() {
    assert_flag(
        "volumes: [\"${S:-./a}:/x\"]",
        FindingKind::HostMount,
        Severity::Medium,
    );
    assert_flag(
        "volumes: [\"${S:-data}:/x\"]",
        FindingKind::HostMount,
        Severity::Medium,
    );
}

#[test]
fn named_and_anonymous_volumes_are_quiet() {
    let out = service("volumes: [\"data:/x\", \"/cache\"]");
    assert!(out.findings.is_empty());
    let mounts = &out.model.service("app").unwrap().mounts;
    assert_eq!(mounts[0].kind, MountKind::Named);
    assert_eq!(mounts[1].kind, MountKind::Anonymous);
}

#[test]
fn malformed_mounts_are_findings() {
    use FindingKind::Unrecognized as U;
    for body in [
        "volumes: [5]",
        "volumes: [[a]]",
        "volumes: {a: b}",
        "volumes: 5",
        "volumes: [\"a:b:c:d\"]",
        "volumes: [\"./x\"]",
        "volumes: [\"${V}\"]",
        "volumes: [\"./a:/x:bogus\"]",
        "volumes:\n  - {source: ./a, target: /x}",
        "volumes:\n  - {type: bind, source: ./a}",
        "volumes:\n  - {type: bind, target: /x}",
        "volumes:\n  - {type: bind, source: [a], target: /x}",
        "volumes:\n  - {type: bind, source: ./a, target: /x, extra: 1}",
        "volumes:\n  - {type: bind, source: ./a, target: /x, bind: 5}",
        "volumes:\n  - {type: bind, source: ./a, target: /x, bind: {odd: 1}}",
    ] {
        assert_flag(body, U, Severity::High);
    }
    // A bare string is the wrong shape, but it is still checked.
    let out = service("volumes: \"/etc:/x\"");
    assert!(!of_kind(&out, U).is_empty());
    assert!(!of_kind(&out, FindingKind::HostMount).is_empty());
}

#[test]
fn propagation_is_reported() {
    assert_flag(
        "volumes: [\"./a:/x:rshared\"]",
        FindingKind::HostMount,
        Severity::Medium,
    );
    assert_flag(
        "volumes:\n  - {type: bind, source: ./a, target: /x, bind: {propagation: shared}}",
        FindingKind::HostMount,
        Severity::Medium,
    );
}

#[test]
fn env_file_mount_is_reported() {
    assert_flag(
        "volumes: [\"./.env:/app/.env:ro\"]",
        FindingKind::EnvFileMount,
        Severity::Medium,
    );
}

#[test]
fn top_level_volume_binding_a_host_path_is_reported() {
    let out = report(
        "services: {}\nvolumes:\n  data:\n    driver_opts: {type: none, o: bind, device: /etc}\n",
    );
    assert_eq!(
        of_kind(&out, FindingKind::HostMount)[0].severity,
        Severity::High
    );
    let out = report("services: {}\nvolumes:\n  data: {external: true}\n");
    assert!(!of_kind(&out, FindingKind::TopLevelResource).is_empty());
    let out = report("services: {}\nvolumes:\n  data: 5\n");
    assert!(!of_kind(&out, FindingKind::Unrecognized).is_empty());
    let out = report("services: {}\nvolumes:\n  data: {odd: 1}\n");
    assert!(!of_kind(&out, FindingKind::Unrecognized).is_empty());
}

#[test]
fn top_level_network_reaching_the_host_is_reported() {
    let out = report("services: {}\nnetworks:\n  n: {driver: host}\n");
    assert_eq!(
        of_kind(&out, FindingKind::TopLevelResource)[0].severity,
        Severity::High
    );
    let out = report("services: {}\nnetworks:\n  n: {name: host, external: true}\n");
    assert!(of_kind(&out, FindingKind::TopLevelResource)
        .iter()
        .any(|f| f.severity == Severity::High));
    let out = report("services: {}\nnetworks: 5\n");
    assert!(!of_kind(&out, FindingKind::Unrecognized).is_empty());
    assert!(
        report("services: {}\nnetworks:\n  n:\n  m: {driver: bridge}\n")
            .findings
            .is_empty()
    );
}

// Ports

fn first_port(body: &str) -> (ComposeReport, projectcheck::compose::Port) {
    let out = service(body);
    let port = out.model.service("app").unwrap().ports[0].clone();
    (out, port)
}

#[test]
fn short_ports_on_all_interfaces() {
    for (spec, target) in [
        ("80", 80),
        ("8080:80", 80),
        ("0.0.0.0:8080:80", 80),
        ("[::]:8080:80", 80),
        (":::8080:80", 80),
        (":8080:80", 80),
        (":80", 80),
        ("${P:-5432}:5432", 5432),
        ("${P-5432}:5432", 5432),
        ("${P}:5432", 5432),
        ("${IP:-0.0.0.0}:80:80", 80),
        ("${SPEC:-8080:80}", 80),
        ("8080:80/udp", 80),
        ("3000-3005", 3000),
        ("9090-9091:8080-8081", 8080),
    ] {
        let (out, port) = first_port(&format!("ports: [\"{spec}\"]"));
        assert_eq!(port.interface, Interface::All, "{spec}");
        assert_eq!(port.target, Some(target), "{spec}");
        assert_eq!(
            of_kind(&out, FindingKind::PortAllInterfaces)[0].severity,
            Severity::High,
            "{spec}"
        );
    }
}

#[test]
fn numeric_port_entry_counts() {
    let (_, port) = first_port("ports: [3000]");
    assert_eq!(port.target, Some(3000));
    assert_eq!(port.interface, Interface::All);
}

#[test]
fn interpolated_port_records_default_and_dynamic() {
    let (_, port) = first_port("ports: [\"${POSTGRES_PORT:-5432}:5432\"]");
    assert_eq!(port.published.as_deref(), Some("5432"));
    assert!(port.dynamic);
}

#[test]
fn loopback_ports_are_info() {
    for spec in [
        "127.0.0.1:80:80",
        "127.0.0.2:80:80",
        "[::1]:80:80",
        "::1:80:80",
    ] {
        let (out, port) = first_port(&format!("ports: [\"{spec}\"]"));
        assert_eq!(port.interface, Interface::Loopback, "{spec}");
        assert_eq!(
            of_kind(&out, FindingKind::PortLoopback)[0].severity,
            Severity::Info
        );
        assert!(of_kind(&out, FindingKind::PortUnknownInterface).is_empty());
    }
}

#[test]
fn variable_loopback_address_is_not_trusted() {
    for spec in ["${IP:-127.0.0.1}:80:80", "${SPEC:-127.0.0.1:80:80}"] {
        let (out, port) = first_port(&format!("ports: [\"{spec}\"]"));
        assert_eq!(port.interface, Interface::Loopback);
        assert_eq!(
            of_kind(&out, FindingKind::PortUnknownInterface)[0].severity,
            Severity::Medium,
            "{spec}"
        );
    }
    // A variable only in the host port does not touch the address.
    let (out, _) = first_port("ports: [\"127.0.0.1:${P:-80}:80\"]");
    assert!(of_kind(&out, FindingKind::PortUnknownInterface).is_empty());
}

#[test]
fn unknown_or_invalid_addresses_fail_closed() {
    for spec in [
        "${IP}:80:80",
        "$IP:80:80",
        "localhost:80:80",
        "127.1:80:80",
        "0:80:80",
    ] {
        let (out, port) = first_port(&format!("ports: [\"{spec}\"]"));
        assert_eq!(port.interface, Interface::Unknown, "{spec}");
        assert_eq!(
            of_kind(&out, FindingKind::PortUnknownInterface)[0].severity,
            Severity::High,
            "{spec}"
        );
    }
}

#[test]
fn specific_address_is_medium() {
    let (out, port) = first_port("ports: [\"192.168.1.5:80:80\"]");
    assert_eq!(port.interface, Interface::Specific("192.168.1.5".into()));
    assert_eq!(
        of_kind(&out, FindingKind::PortSpecificInterface)[0].severity,
        Severity::Medium
    );
    // A v4-mapped address is not treated as loopback.
    let (_, port) = first_port("ports: [\"[::ffff:127.0.0.1]:80:80\"]");
    assert!(matches!(port.interface, Interface::Specific(_)));
}

#[test]
fn long_ports() {
    let (out, port) = first_port("ports:\n  - {target: 80, published: 8080}");
    assert_eq!(port.interface, Interface::All);
    assert_eq!(port.target, Some(80));
    assert_eq!(port.published.as_deref(), Some("8080"));
    assert_eq!(of_kind(&out, FindingKind::PortAllInterfaces).len(), 1);

    let (_, port) = first_port("ports:\n  - {target: 80}");
    assert_eq!(port.interface, Interface::All);
    assert_eq!(port.published, None);

    let (_, port) = first_port("ports:\n  - {target: 80, host_ip: 0.0.0.0, published: \"8080\"}");
    assert_eq!(port.interface, Interface::All);

    let (_, port) = first_port("ports:\n  - {target: \"80\", host_ip: \"::\"}");
    assert_eq!(port.interface, Interface::All);

    let (_, port) = first_port("ports:\n  - {target: 80, published: \"${P:-5432}\"}");
    assert_eq!(port.interface, Interface::All);
    assert_eq!(port.published.as_deref(), Some("5432"));
    assert!(port.dynamic);

    let (out, port) = first_port("ports:\n  - {target: 80, host_ip: 127.0.0.1}");
    assert_eq!(port.interface, Interface::Loopback);
    assert!(of_kind(&out, FindingKind::PortUnknownInterface).is_empty());

    let (out, port) = first_port("ports:\n  - {target: 80, host_ip: \"${I:-127.0.0.1}\"}");
    assert_eq!(port.interface, Interface::Loopback);
    assert_eq!(
        of_kind(&out, FindingKind::PortUnknownInterface)[0].severity,
        Severity::Medium
    );

    let (_, port) = first_port("ports:\n  - {target: 80, host_ip: \"${I}\"}");
    assert_eq!(port.interface, Interface::Unknown);

    let (_, port) = first_port("ports:\n  - {target: 80, host_ip: localhost}");
    assert_eq!(port.interface, Interface::Unknown);

    let (_, port) = first_port("ports:\n  - {target: 80, host_ip: [a]}");
    assert_eq!(port.interface, Interface::Unknown);
}

#[test]
fn malformed_ports_are_findings() {
    use FindingKind::Unrecognized as U;
    for body in [
        "ports: {a: 1}",
        "ports: true",
        "ports: [true]",
        "ports: [1.5]",
        "ports: [-80]",
        "ports: [[80]]",
        "ports: [\"abc\"]",
        "ports: [\"70000:80\"]",
        "ports: [\"80:70000\"]",
        "ports: [\"0:80\"]",
        "ports: [\"80:0\"]",
        "ports: [\"80:80/icmp\"]",
        "ports: [\"80-70:80\"]",
        "ports: [\"8000-8001:80-90\"]",
        "ports: [\"${SPEC}\"]",
        "ports: [\"8080:${T}\"]",
        "ports:\n  - {published: 80}",
        "ports:\n  - {target: abc}",
        "ports:\n  - {target: 70000}",
        "ports:\n  - {target: [80]}",
        "ports:\n  - {target: 80, published: abc}",
        "ports:\n  - {target: 80, published: [8080]}",
        "ports:\n  - {target: 80, protocol: icmp}",
        "ports:\n  - {target: 80, surprise: 1}",
    ] {
        assert_flag(body, U, Severity::High);
    }
}

#[test]
fn ports_string_shape_is_reported_and_still_counted() {
    let (out, port) = first_port("ports: \"8080:80\"");
    assert_eq!(port.interface, Interface::All);
    assert!(!of_kind(&out, FindingKind::Unrecognized).is_empty());
}

// Secrets, configs, build, env_file, include, extends, profiles

#[test]
fn secrets_and_configs_files() {
    for (section, kind) in [
        ("secrets", FindingKind::Secret),
        ("configs", FindingKind::Config),
    ] {
        for file in [
            "/etc/shadow",
            "~/.aws/credentials",
            "../outside",
            "./a/../../b",
            "${F}",
        ] {
            let out = report(&format!(
                "services: {{}}\n{section}:\n  s:\n    file: \"{file}\"\n"
            ));
            assert_eq!(of_kind(&out, kind)[0].severity, Severity::High, "{file}");
        }
        let out = report(&format!(
            "services: {{}}\n{section}:\n  s:\n    file: ./inside\n"
        ));
        assert_eq!(of_kind(&out, kind)[0].severity, Severity::Info);
        let out = report(&format!(
            "services: {{}}\n{section}:\n  s:\n    environment: FOO\n"
        ));
        assert_eq!(of_kind(&out, kind)[0].severity, Severity::Medium);
        let out = report(&format!(
            "services: {{}}\n{section}:\n  s:\n    external: true\n"
        ));
        assert_eq!(of_kind(&out, kind)[0].severity, Severity::Medium);
        for body in ["5", "{odd: 1}", "{file: [a]}", "{}"] {
            let out = report(&format!("services: {{}}\n{section}:\n  s: {body}\n"));
            assert!(
                !of_kind(&out, FindingKind::Unrecognized).is_empty(),
                "{body}"
            );
        }
        let out = report(&format!("services: {{}}\n{section}: 5\n"));
        assert!(!of_kind(&out, FindingKind::Unrecognized).is_empty());
    }
}

#[test]
fn inline_config_content_is_not_echoed() {
    let out = report("services: {}\nconfigs:\n  c:\n    content: hunter2\n");
    assert!(!format!("{out:?}").contains("hunter2"));
}

#[test]
fn build_contexts() {
    use FindingKind::BuildContext as B;
    for context in ["/etc", "~/x", "..", "../x", "./a/../../b", "${CTX}"] {
        assert_flag(&format!("build: \"{context}\""), B, Severity::High);
        assert_flag(
            &format!("build:\n  context: \"{context}\""),
            B,
            Severity::High,
        );
    }
    assert_flag("build: \"${CTX:-./api}\"", B, Severity::Medium);
    assert_flag("build: https://example.com/repo.git", B, Severity::Medium);
    assert_clean("build: ./api");
    assert_clean("build:\n  context: ./api\n  dockerfile: ../Dockerfile\n  args: {A: b}");
    assert_flag(
        "build:\n  context: ./api\n  dockerfile: ../../Dockerfile",
        B,
        Severity::High,
    );
    assert_flag("build:\n  dockerfile: /etc/x", B, Severity::High);
}

#[test]
fn additional_contexts() {
    use FindingKind::BuildContext as B;
    assert_flag(
        "build:\n  context: .\n  additional_contexts: {a: /etc}",
        B,
        Severity::High,
    );
    assert_flag(
        "build:\n  context: .\n  additional_contexts: {a: ../x}",
        B,
        Severity::High,
    );
    assert_flag(
        "build:\n  context: .\n  additional_contexts: [\"a=/etc\"]",
        B,
        Severity::High,
    );
    assert_flag(
        "build:\n  context: .\n  additional_contexts: [\"a=../x\"]",
        B,
        Severity::High,
    );
    assert_flag(
        "build:\n  context: .\n  additional_contexts: {a: \"oci-layout:///etc\"}",
        B,
        Severity::High,
    );
    assert_clean("build:\n  context: .\n  additional_contexts: {a: ./lib, b: \"docker-image://x\", c: \"service:d\"}");
    assert_flag(
        "build:\n  context: .\n  additional_contexts: 5",
        FindingKind::Unrecognized,
        Severity::High,
    );
    assert_flag(
        "build:\n  context: .\n  additional_contexts: [noequals]",
        FindingKind::Unrecognized,
        Severity::High,
    );
    assert_flag(
        "build:\n  context: .\n  additional_contexts: {a: [x]}",
        FindingKind::Unrecognized,
        Severity::High,
    );
}

#[test]
fn build_options_and_shapes() {
    use FindingKind::{BuildOption as O, Unrecognized as U};
    assert_flag("build:\n  context: .\n  ssh: [default]", O, Severity::High);
    assert_flag(
        "build:\n  context: .\n  privileged: true",
        O,
        Severity::Critical,
    );
    assert_flag("build:\n  context: .\n  network: host", O, Severity::High);
    assert_flag(
        "build:\n  context: .\n  entitlements: [network.host]",
        O,
        Severity::High,
    );
    assert_flag("build: 5", U, Severity::High);
    assert_flag("build: [a]", U, Severity::High);
    assert_flag("build:\n  context: [a]", U, Severity::High);
    assert_flag("build:\n  context: .\n  odd: 1", U, Severity::High);
    assert_flag("build:\n  dockerfile: [a]", U, Severity::High);
}

#[test]
fn env_file_forms() {
    use FindingKind::{EnvFile as E, Unrecognized as U};
    assert_flag("env_file: .env", E, Severity::Medium);
    assert_flag("env_file: [./a.env, ./b.env]", E, Severity::Medium);
    assert_flag(
        "env_file:\n  - {path: ./a.env, required: false}",
        E,
        Severity::Medium,
    );
    assert_flag("env_file: /etc/passwd", E, Severity::High);
    assert_flag("env_file: [\"~/.env\"]", E, Severity::High);
    assert_flag("env_file:\n  - path: ../up.env", E, Severity::High);
    assert_flag("env_file: \"${F}\"", E, Severity::High);
    assert_flag("env_file: 5", U, Severity::High);
    assert_flag("env_file: [5]", U, Severity::High);
    assert_flag("env_file:\n  - {required: true}", U, Severity::High);
    assert_flag("env_file:\n  - {path: [a]}", U, Severity::High);
    assert_flag("env_file:\n  - {path: ./a, odd: 1}", U, Severity::High);
}

#[test]
fn include_forms() {
    use FindingKind::{Include as I, Unrecognized as U};
    for text in [
        "include: [./other.yml]",
        "include: [/etc/x.yml]",
        "include: [../x.yml]",
        "include: [\"oci://example/x\"]",
        "include:\n  - path: ./a.yml",
        "include:\n  - path: [./a.yml, ./b.yml]",
    ] {
        let out = report(text);
        assert!(!of_kind(&out, I).is_empty(), "{text}");
        assert!(of_kind(&out, I)
            .iter()
            .all(|f| f.severity == Severity::High));
        assert!(of_kind(&out, U).is_empty(), "{text}: {:#?}", out.findings);
    }
    let out = report("include:\n  - path: ./a.yml\n    project_directory: /etc\n");
    assert!(of_kind(&out, I)
        .iter()
        .any(|f| f.message.contains("project directory")));
    let out = report("include:\n  - path: ./a.yml\n    env_file: /etc/e\n");
    assert_eq!(
        of_kind(&out, FindingKind::EnvFile)[0].severity,
        Severity::High
    );
    for text in [
        "include: ./a.yml",
        "include: 5",
        "include: [5]",
        "include:\n  - odd: 1",
        "include:\n  - path: 5",
        "include:\n  - path: [5]",
    ] {
        assert!(!of_kind(&report(text), U).is_empty(), "{text}");
    }
}

#[test]
fn extends_forms() {
    use FindingKind::{Extends as E, Unrecognized as U};
    assert_flag("extends: base", E, Severity::Info);
    assert_flag("extends: {service: base}", E, Severity::Info);
    for file in [
        "./common.yml",
        "/etc/x.yml",
        "../x.yml",
        "${F}",
        "https://e.com/x.yml",
    ] {
        assert_flag(
            &format!("extends: {{service: base, file: \"{file}\"}}"),
            E,
            Severity::High,
        );
    }
    assert_flag("extends: 5", U, Severity::High);
    assert_flag("extends: {file: ./a.yml}", U, Severity::High);
    assert_flag("extends: {service: a, odd: 1}", U, Severity::High);
    assert_flag("extends: {service: a, file: 5}", U, Severity::High);
}

#[test]
fn profiles_are_reported_per_service() {
    let out = report(
        "services:\n  a: {image: x, profiles: [p1, p2]}\n  b: {image: x}\n  c: {image: x, profiles: [p3]}\n",
    );
    let found = of_kind(&out, FindingKind::Profile);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].service.as_deref(), Some("a"));
    assert!(found[0].message.contains("p1") && found[0].message.contains("p2"));
    assert_eq!(found[1].service.as_deref(), Some("c"));
    assert_eq!(out.model.service("a").unwrap().profiles, vec!["p1", "p2"]);
    let defaults: Vec<&str> = out
        .model
        .default_services()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(defaults, vec!["b"]);
}

#[test]
fn malformed_profiles_are_findings_and_still_exclude_from_default() {
    let out = service("profiles: dev");
    assert!(!of_kind(&out, FindingKind::Unrecognized).is_empty());
    assert_eq!(out.model.service("app").unwrap().profiles, vec!["dev"]);
    assert_flag(
        "profiles: {a: 1}",
        FindingKind::Unrecognized,
        Severity::High,
    );
    assert_flag("profiles: [[a]]", FindingKind::Unrecognized, Severity::High);
}

// Model

fn replicas(body: &str) -> projectcheck::compose::Replicas {
    service(body).model.service("app").unwrap().replicas.clone()
}

#[test]
fn replica_forms() {
    assert_eq!(replicas("command: x").count, Some(1));
    assert_eq!(replicas("deploy: {replicas: 3}").count, Some(3));
    assert_eq!(replicas("deploy: {replicas: \"4\"}").count, Some(4));
    assert_eq!(replicas("scale: 5").count, Some(5));
    let expr = replicas("deploy: {replicas: \"${R:-2}\"}");
    assert_eq!(expr.count, Some(2));
    assert!(expr.dynamic);
    assert_eq!(expr.expression.as_deref(), Some("${R:-2}"));
    assert_eq!(replicas("scale: 9\ndeploy: {replicas: 3}").count, Some(3));
    assert_eq!(replicas("deploy: {replicas: 3}\nscale: 9").count, Some(3));
}

#[test]
fn bad_replicas_are_findings() {
    for body in [
        "deploy: {replicas: -1}",
        "deploy: {replicas: 1.5}",
        "deploy: {replicas: [1]}",
        "deploy: {replicas: abc}",
        "deploy: {replicas: \"${R}\"}",
        "deploy: {replicas: 99999999999}",
        "scale: true",
    ] {
        assert_eq!(replicas(body).count, None, "{body}");
        assert_flag(body, FindingKind::Unrecognized, Severity::High);
    }
    assert_flag(
        "deploy: {replicas: 100000}",
        FindingKind::Weakening,
        Severity::Medium,
    );
}

#[test]
fn shared_environment_keys() {
    let out = report(
        "x-env: &env\n  SHARED: 1\n  ALSO: 2\nservices:\n  a:\n    image: x\n    environment: *env\n  b:\n    image: x\n    environment: [SHARED=1, ONLY_B=2, ALSO]\n  c:\n    image: x\n    environment: {OTHER: 1}\n",
    );
    let shared: Vec<(&str, Vec<&str>)> = out
        .model
        .shared_env
        .iter()
        .map(|s| {
            (
                s.key.as_str(),
                s.services.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    assert_eq!(
        shared,
        vec![("ALSO", vec!["a", "b"]), ("SHARED", vec!["a", "b"])]
    );
}

#[test]
fn environment_values_never_reach_the_output() {
    let out = service("environment:\n  API_KEY: hunter2\n  OTHER: ${X:-hunter3}\n");
    let list = service("environment: [\"API_KEY=hunter2\"]");
    for printed in [format!("{out:?}"), format!("{list:?}")] {
        assert!(!printed.contains("hunter"), "{printed}");
    }
    assert_eq!(
        out.model.service("app").unwrap().env_keys,
        vec!["API_KEY", "OTHER"]
    );
}

#[test]
fn malformed_environment_is_reported_without_echoing_it() {
    assert_flag("environment: 5", FindingKind::Unrecognized, Severity::High);
    assert_flag(
        "environment: [5]",
        FindingKind::Unrecognized,
        Severity::High,
    );
    let out = service("environment: [\"pass word hunter2\"]");
    assert!(!of_kind(&out, FindingKind::Unrecognized).is_empty());
    assert!(!format!("{out:?}").contains("hunter2"));
}

// Hostile documents

/// The file must fail quickly. A hang would fail by timeout in the runner, and
/// this also catches a slow pass.
fn assert_fast_error(text: &str) {
    let start = Instant::now();
    let result = parse_str(text);
    assert!(result.is_err(), "bomb was accepted");
    assert!(
        start.elapsed() < Duration::from_secs(30),
        "took {:?}",
        start.elapsed()
    );
}

#[test]
fn classic_alias_bomb_is_an_error() {
    let mut text = String::from("x-a0: &a0 [lol, lol, lol, lol, lol, lol, lol, lol, lol]\n");
    for level in 1..30 {
        let prev = level - 1;
        let refs = vec![format!("*a{prev}"); 9].join(", ");
        text.push_str(&format!("x-a{level}: &a{level} [{refs}]\n"));
    }
    text.push_str("services:\n  app:\n    image: x\n    command: *a29\n");
    assert_fast_error(&text);
}

#[test]
fn wide_alias_bomb_under_the_parser_jump_limit_is_too_complex() {
    // Few jumps compared with events, so only a node budget catches this.
    let scalars = vec!["x"; 150_000].join(",");
    let aliases = vec!["*a"; 150_000].join(",");
    let text = format!("x-a: &a [{scalars}]\nx-b: [{aliases}]\nservices: {{}}\n");
    assert!((text.len() as u64) < MAX_COMPOSE_BYTES);
    let start = Instant::now();
    assert!(matches!(parse_str(&text), Err(Error::TooComplex { .. })));
    assert!(start.elapsed() < Duration::from_secs(30));
}

#[test]
fn merge_key_bomb_is_an_error() {
    let mut text = String::from("x-a0: &a0 {a: 1, b: 2, c: 3}\n");
    for level in 1..30 {
        let prev = level - 1;
        text.push_str(&format!(
            "x-a{level}: &a{level} {{l: *a{prev}, m: *a{prev}, n: *a{prev}, o: *a{prev}}}\n"
        ));
    }
    text.push_str("services:\n  app:\n    <<: *a29\n");
    assert_fast_error(&text);
}

#[test]
fn deep_flow_nesting_is_an_error() {
    for (open, close) in [("[", "]"), ("{", "}"), ("[a, ", "]"), ("{a: ", "}")] {
        let text = format!(
            "services: {}{}",
            open.repeat(100_000),
            close.repeat(100_000)
        );
        assert_fast_error(&text);
    }
}

#[test]
fn flow_nesting_hidden_behind_quotes_is_an_error() {
    // Closers in quotes must not cancel out the real openers.
    let text = format!("services: {}", "[it's, \" ' ]] \", ".repeat(60_000));
    assert_fast_error(&text);
}

#[test]
fn nesting_at_the_opener_limit_stays_fast() {
    // As many openers as the limit allows, then as many tokens as fit.
    let openers = "[".repeat(1_900);
    let items = "x,".repeat(500_000);
    let text = format!("x-a: {openers}{items}");
    assert!((text.len() as u64) < MAX_COMPOSE_BYTES);
    assert_fast_error(&text);
}

#[test]
fn deep_block_nesting_is_an_error() {
    assert_fast_error(&format!("services: {}x", "- ".repeat(450_000)));
    let mut text = String::from("services:\n");
    for depth in 1..1_300 {
        text.push_str(&" ".repeat(depth));
        text.push_str("a:\n");
    }
    assert_fast_error(&text);
}

#[test]
fn large_flat_documents_finish_quickly() {
    let start = Instant::now();
    let keys: String = (0..60_000).map(|i| format!("x-{i}: 1\n")).collect();
    assert!(parse_str(&format!("{keys}services: {{}}\n")).is_ok());
    let scalar = format!("services: {{}}\nx-a: \"{}\"\n", "y".repeat(1_000_000));
    assert!(parse_str(&scalar).is_ok());
    let anchors: String = (0..50_000).map(|i| format!("x-{i}: &a{i} 1\n")).collect();
    assert!(parse_str(&format!("{anchors}services: {{}}\n")).is_ok());
    assert!(
        start.elapsed() < Duration::from_secs(30),
        "took {:?}",
        start.elapsed()
    );
}

#[test]
fn self_referencing_alias_is_an_error() {
    assert_fast_error("x-a: &a [*a]\nservices: {}\n");
}

#[test]
fn oversized_input_is_an_error() {
    let text = format!(
        "# {}\nservices: {{}}\n",
        "x".repeat(MAX_COMPOSE_BYTES as usize)
    );
    assert!(matches!(parse_str(&text), Err(Error::InputTooLarge { .. })));
}

#[test]
fn invalid_documents_are_errors() {
    for text in [
        "a: [",
        "- a\n- b\n",
        "just text",
        "",
        "services: {}\n---\nservices:\n  evil: {privileged: true}\n",
        "services: {a: {image: x}, a: {privileged: true}}\n",
        "services:\n  a:\n    privileged: false\n    privileged: true\n",
        "services:\n  a:\n    <<: 5\n",
    ] {
        assert!(parse_str(text).is_err(), "accepted {text:?}");
    }
}

#[test]
fn hostile_text_is_safe_to_print() {
    let out = report(
        "services:\n  \"a\\e[31m\\nb\":\n    image: \"x\\e[2J\"\n    profiles: [\"p\\e[0m\"]\n    devices: [\"/dev/\\e]0;own\\a\"]\n    Odd\\u202e: 1\n",
    );
    for finding in &out.findings {
        assert!(!finding.message.contains('\u{1b}'));
        assert!(!finding.message.contains('\n'));
        assert!(!finding.service.as_deref().unwrap_or("").contains('\u{1b}'));
    }
    let service = &out.model.services[0];
    assert!(!service.name.contains('\u{1b}'));
    assert!(!service.image.as_deref().unwrap_or("").contains('\u{1b}'));
    assert!(!service.profiles[0].contains('\u{1b}'));
    assert!(
        !of_kind(&out, FindingKind::Unrecognized).is_empty(),
        "the name is invalid"
    );
}

#[test]
fn huge_finding_count_is_cut_and_says_so() {
    let ports = vec!["\"80\""; 6000].join(",");
    let out = service(&format!("ports: [{ports}]"));
    assert!(out.findings.len() <= 5001);
    let last = out.findings.last().unwrap();
    assert_eq!(last.severity, Severity::Critical);
    assert!(last.message.contains("more than"));
}

#[test]
fn variables_never_come_from_the_host_environment() {
    // PATH is set in every test environment, so reading it would change this.
    let out = service("volumes: [\"${PATH}:/x\"]");
    let mount = &out.model.service("app").unwrap().mounts[0];
    assert_eq!(mount.kind, MountKind::Host);
    assert_eq!(mount.source.as_deref(), Some("<variable>"));
}

// Files on disk

fn write(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn parse_file_enforces_the_size_limit() {
    let dir = TempDir::new().unwrap();
    let text = format!("# {}\n", "x".repeat(MAX_COMPOSE_BYTES as usize + 1));
    write(dir.path(), "compose.yaml", &text);
    assert!(matches!(
        parse_file(dir.path(), Path::new("compose.yaml")),
        Err(Error::TooLarge { .. })
    ));
}

#[test]
fn parse_file_refuses_non_utf8() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("compose.yaml"), [0xff, 0xfe, 0x00]).unwrap();
    assert!(matches!(
        parse_file(dir.path(), Path::new("compose.yaml")),
        Err(Error::NotUtf8(_))
    ));
}

#[test]
fn parse_file_refuses_env_files_and_paths_outside_the_project() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), ".env", "services: {}\n");
    write(dir.path(), "prod.env", "services: {}\n");
    write(dir.path(), ".env.local", "services: {}\n");
    for name in [".env", "prod.env", ".env.local"] {
        assert!(matches!(
            parse_file(dir.path(), Path::new(name)),
            Err(Error::EnvFileRefused(_))
        ));
    }
    let outer = TempDir::new().unwrap();
    write(outer.path(), "other.yml", "services: {}\n");
    let escape = format!(
        "../{}/other.yml",
        outer.path().file_name().unwrap().to_str().unwrap()
    );
    assert!(matches!(
        parse_file(dir.path(), Path::new(&escape)),
        Err(Error::PathEscapes(_))
    ));
    assert!(matches!(
        parse_file(dir.path(), &outer.path().join("other.yml")),
        Err(Error::PathEscapes(_))
    ));
}

#[test]
fn compose_file_in_a_subdirectory_resolves_paths_from_there() {
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "deploy/compose.yaml",
        "services:\n  a:\n    image: x\n    volumes: [\"../data:/inside\", \"../../up:/outside\"]\n",
    );
    let out = parse_file(dir.path(), Path::new("deploy/compose.yaml")).unwrap();
    let mounts = &out.model.service("a").unwrap().mounts;
    assert_eq!(mounts[0].kind, MountKind::Project);
    assert_eq!(mounts[1].kind, MountKind::Host);
}

#[test]
fn check_project_reports_override_files() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "compose.yaml", "services:\n  a: {image: x}\n");
    let file = Path::new("compose.yaml");
    assert!(check_project(dir.path(), file).unwrap().findings.is_empty());

    write(
        dir.path(),
        "compose.override.yaml",
        "services:\n  a: {privileged: true}\n",
    );
    let out = check_project(dir.path(), file).unwrap();
    assert_eq!(of_kind(&out, FindingKind::OverrideFile).len(), 1);
    let privileged = of_kind(&out, FindingKind::Privileged);
    assert_eq!(privileged.len(), 1);
    assert!(privileged[0].message.starts_with("[compose.override.yaml]"));

    write(dir.path(), "compose.override.yaml", "a: [");
    let out = check_project(dir.path(), file).unwrap();
    assert!(of_kind(&out, FindingKind::Unrecognized)
        .iter()
        .any(|f| f.severity == Severity::Critical));
}

#[cfg(unix)]
mod links {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn parse_file_refuses_a_link_out_or_to_an_env_file() {
        let outer = TempDir::new().unwrap();
        write(outer.path(), "evil.yml", "services: {}\n");
        let dir = TempDir::new().unwrap();
        symlink(
            outer.path().join("evil.yml"),
            dir.path().join("compose.yml"),
        )
        .unwrap();
        assert!(matches!(
            parse_file(dir.path(), Path::new("compose.yml")),
            Err(Error::PathEscapes(_))
        ));

        write(dir.path(), ".env", "services: {}\n");
        symlink(dir.path().join(".env"), dir.path().join("compose.yaml")).unwrap();
        assert!(matches!(
            parse_file(dir.path(), Path::new("compose.yaml")),
            Err(Error::EnvFileRefused(_))
        ));
    }

    #[test]
    fn override_link_out_of_the_project_is_critical() {
        let outer = TempDir::new().unwrap();
        write(outer.path(), "evil.yml", "services: {}\n");
        let dir = TempDir::new().unwrap();
        write(dir.path(), "compose.yaml", "services: {}\n");
        symlink(
            outer.path().join("evil.yml"),
            dir.path().join("compose.override.yml"),
        )
        .unwrap();
        let out = check_project(dir.path(), Path::new("compose.yaml")).unwrap();
        assert_eq!(of_kind(&out, FindingKind::OverrideFile).len(), 1);
        assert!(of_kind(&out, FindingKind::Unrecognized)
            .iter()
            .any(|f| f.severity == Severity::Critical));
    }

    #[test]
    fn project_paths_that_link_out_are_reported() {
        let outer = TempDir::new().unwrap();
        let dir = TempDir::new().unwrap();
        symlink(outer.path(), dir.path().join("link")).unwrap();
        write(
            dir.path(),
            "compose.yaml",
            "services:\n  a:\n    image: x\n    volumes: [\"./link:/x\"]\n    build: ./link\n    env_file: ./link\n    extends: {service: b, file: ./link}\nsecrets:\n  s: {file: ./link}\n",
        );
        let out = parse_file(dir.path(), Path::new("compose.yaml")).unwrap();
        assert_eq!(of_kind(&out, FindingKind::SymlinkEscape).len(), 5);

        // The same text with no disk access cannot see the link.
        let text = fs::read_to_string(dir.path().join("compose.yaml")).unwrap();
        assert!(of_kind(&parse_str(&text).unwrap(), FindingKind::SymlinkEscape).is_empty());
    }

    #[test]
    fn parse_file_does_not_read_fifos() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("compose.yaml");
        let status = std::process::Command::new("mkfifo").arg(&path).status();
        if !status.is_ok_and(|s| s.success()) {
            eprintln!("skipped: mkfifo is not available");
            return;
        }
        // Opening a FIFO would hang forever, so returning at all is the test.
        assert!(matches!(
            parse_file(dir.path(), Path::new("compose.yaml")),
            Err(Error::NotARegularFile(_))
        ));
    }
}
