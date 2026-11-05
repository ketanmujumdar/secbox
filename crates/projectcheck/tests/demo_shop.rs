//! Runs the check on a copy of a real Compose file, when it exists on this
//! machine. Only that one file is read, so the test skips everywhere else.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use projectcheck::compose::{check_project, FindingKind, Interface, MountKind, Severity};
use projectcheck::detect::{detect, ProjectKind};
use tempfile::TempDir;

const REAL_FILE: &str = "/Users/you/code/demo_shop/docker-compose.yml";

#[test]
fn real_compose_file() {
    if !Path::new(REAL_FILE).exists() {
        eprintln!("skipped: {REAL_FILE} does not exist");
        return;
    }
    let dir = TempDir::new().unwrap();
    fs::copy(REAL_FILE, dir.path().join("docker-compose.yml")).unwrap();

    let detection = detect(dir.path()).unwrap();
    let ProjectKind::Compose { file, overrides } = detection.kind else {
        panic!("not detected as Compose: {:?}", detection.kind);
    };
    assert!(overrides.is_empty());

    let report = check_project(dir.path(), &file).unwrap();
    let model = &report.model;

    let defaults: BTreeSet<&str> = model.default_services().map(|s| s.name.as_str()).collect();
    let expected: BTreeSet<&str> = ["postgres", "seed", "api", "worker", "web", "s3", "mailpit"]
        .into_iter()
        .collect();
    assert_eq!(defaults, expected);

    let worker = model.service("worker").unwrap();
    assert_eq!(worker.replicas.count, Some(2));
    assert!(worker.replicas.dynamic);
    assert_eq!(
        worker.replicas.expression.as_deref(),
        Some("${WORKER_REPLICAS:-2}")
    );
    assert_eq!(model.service("api").unwrap().replicas.count, Some(1));

    assert_eq!(model.default_ports_on_all_interfaces(), 7);
    let postgres_port = &model.service("postgres").unwrap().ports[0];
    assert_eq!(postgres_port.target, Some(5432));
    assert_eq!(postgres_port.published.as_deref(), Some("5432"));
    assert_eq!(postgres_port.interface, Interface::All);
    assert!(postgres_port.dynamic);

    let default_port_findings = report
        .findings
        .iter()
        .filter(|f| f.kind == FindingKind::PortAllInterfaces)
        .filter(|f| !f.service.as_deref().unwrap_or("").starts_with("documenso"))
        .count();
    assert_eq!(default_port_findings, 7);

    for name in ["documenso", "documenso-bucket"] {
        assert_eq!(model.service(name).unwrap().profiles, vec!["documenso"]);
        assert!(report
            .findings
            .iter()
            .any(|f| { f.kind == FindingKind::Profile && f.service.as_deref() == Some(name) }));
    }

    // Three repo files are bind-mounted read-only, one only with the profile.
    let project_mounts: Vec<(&str, bool)> = model
        .services
        .iter()
        .flat_map(|s| s.mounts.iter().map(move |m| (s.name.as_str(), m)))
        .filter(|(_, m)| m.kind == MountKind::Project)
        .map(|(name, m)| (name, m.read_only))
        .collect();
    assert_eq!(
        project_mounts,
        vec![("postgres", true), ("s3", true), ("documenso", true)]
    );

    let shared = model
        .shared_env
        .iter()
        .find(|s| s.key == "DATABASE_URL")
        .unwrap();
    assert_eq!(shared.services, vec!["api", "seed", "worker"]);

    // Nothing in this ordinary file should look unknown or dangerous.
    assert!(
        report
            .findings
            .iter()
            .all(|f| f.kind != FindingKind::Unrecognized),
        "{:#?}",
        report.findings
    );
    assert!(report
        .findings
        .iter()
        .all(|f| f.severity < Severity::Critical));
    assert!(report
        .findings
        .iter()
        .all(|f| f.kind != FindingKind::HostMount));

    // No value from the environment blocks reaches the output.
    let printed = format!("{report:?}");
    assert!(!printed.contains("example-local-value"));
    assert!(!printed.contains("example-admin-value"));
}
