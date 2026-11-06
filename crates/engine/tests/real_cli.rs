//! Runs against the real CLI in the project's `.tools`, with state in the default
//! directory. Run with: cargo test -p engine --test real_cli -- --ignored
//! The lifecycle test boots and removes one small VM.

use std::path::{Path, PathBuf};

use engine::{
    CreateSpec, Engine, EngineError, ExecRequest, GuestPath, LogOptions, MsbEngine, MsbLocation,
    NetworkPolicy, Resources, SandboxName, SandboxStatus,
};

fn project_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate is two levels below the project root")
        .to_owned()
}

fn engine() -> MsbEngine {
    let user_home = std::env::var_os("HOME").expect("HOME is set");
    MsbEngine::new(MsbLocation::ProjectTools {
        project_root: project_root(),
        home: MsbEngine::default_home(Path::new(&user_home)),
    })
    .expect("install the CLI into .tools/msb first")
}

#[test]
#[ignore = "needs the CLI in .tools"]
fn version_is_the_tested_one() {
    assert_eq!(engine().version().unwrap(), engine::TESTED_VERSION);
}

#[test]
#[ignore = "needs the CLI in .tools"]
fn doctor_reports_ready() {
    assert!(engine().doctor().unwrap().contains("Host setup is ready"));
}

#[test]
#[ignore = "needs the CLI in .tools"]
fn status_of_unknown_sandbox_is_not_found() {
    let name = SandboxName::new("no-such-sandbox").unwrap();
    assert_eq!(engine().status(&name).unwrap(), SandboxStatus::NotFound);
}

/// A state directory inside this project is too long for the CLI's sockets. This
/// fails if the project moves to a short enough path, which would be fine to allow.
#[test]
#[ignore = "needs the CLI in .tools"]
fn preflight_accepts_the_default_home_and_refuses_one_in_the_project() {
    engine().preflight().unwrap();
    let in_project = MsbEngine::new(MsbLocation::ProjectTools {
        project_root: project_root(),
        home: project_root().join(".tools/msb"),
    })
    .unwrap();
    assert!(matches!(
        in_project.preflight(),
        Err(EngineError::HomePathTooLong { .. })
    ));
}

/// Removes the sandbox even if an assertion fails, so that nothing is left running.
struct Cleanup(SandboxName);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = engine().destroy(&self.0);
    }
}

#[test]
#[ignore = "boots a small VM"]
fn full_lifecycle_against_a_real_vm() {
    let e = engine();
    let name = SandboxName::new("engine-itest").unwrap();
    let _cleanup = Cleanup(name.clone());
    let _ = e.destroy(&name);

    e.create(&CreateSpec {
        name: name.clone(),
        image: "alpine".to_owned(),
        resources: Resources::new(256, 1, 2).unwrap(),
        network: NetworkPolicy::DenyAll,
        secrets: vec![],
        ports: vec![],
        previous_disk: None,
    })
    .unwrap();
    assert_eq!(e.status(&name).unwrap(), SandboxStatus::Running);

    let mut req = ExecRequest::new(["sh", "-c", "echo $A; pwd; echo err >&2; exit 7"]);
    req.env = vec![("A".to_owned(), "b c".to_owned())];
    req.workdir = Some(GuestPath::new("/tmp").unwrap());
    let out = e.exec(&name, &req).unwrap();
    assert_eq!(out.code, 7);
    assert_eq!(out.stdout, "b c\n/tmp\n");
    assert_eq!(out.stderr, "err\n");

    let dir = std::env::temp_dir().join(format!("engine-itest-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("f"), "payload").unwrap();
    e.copy_in(&name, &dir, &GuestPath::new("/tmp/d").unwrap())
        .unwrap();
    let out = e
        .exec(&name, &ExecRequest::new(["cat", "/tmp/d/f"]))
        .unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(out.stdout, "payload");

    let job = e
        .exec_detached(&name, &ExecRequest::new(["sleep", "30"]))
        .unwrap();
    assert!(job.starts_with("job_"), "{job}");
    assert!(e.logs(&name, &LogOptions { tail: Some(5) }).is_ok());

    let mut slow = ExecRequest::new(["sleep", "20"]);
    slow.timeout = Some(std::time::Duration::from_secs(1));
    assert!(matches!(
        e.exec(&name, &slow),
        Err(EngineError::Timeout { .. })
    ));

    e.stop(&name).unwrap();
    assert_eq!(e.status(&name).unwrap(), SandboxStatus::Stopped);
    assert!(matches!(
        e.exec(&name, &ExecRequest::new(["true"])),
        Err(EngineError::InvalidState { .. })
    ));
    e.start(&name).unwrap();
    assert_eq!(e.status(&name).unwrap(), SandboxStatus::Running);

    e.destroy(&name).unwrap();
    assert_eq!(e.status(&name).unwrap(), SandboxStatus::NotFound);
    assert!(matches!(
        e.destroy(&name),
        Err(EngineError::NotFound { .. })
    ));
}
