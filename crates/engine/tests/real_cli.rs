//! Runs against the real CLI in the project's `.tools`, with state in the default
//! directory. Run with: cargo test -p engine --test real_cli -- --ignored
//! The lifecycle test boots and removes one small VM.

use std::path::{Path, PathBuf};

use engine::{
    CreateSpec, Engine, EngineError, ExecRequest, GuestPath, LogOptions, MsbEngine, MsbLocation,
    NetworkPolicy, ResizeSpec, Resources, SandboxName, SandboxStatus,
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

/// The fake cannot show what the engine accepts: it was this test's commands
/// that found the engine refuses a disk of the size the sandbox already has.
#[test]
#[ignore = "boots a small VM"]
fn resizing_a_stopped_sandbox_keeps_its_data_and_reports_the_new_limits() {
    let engine = engine();
    let name = SandboxName::new("resize-itest").unwrap();
    let _ = engine.destroy(&name);
    let _cleanup = Cleanup(name.clone());
    engine
        .create(&CreateSpec {
            name: name.clone(),
            image: "alpine".to_owned(),
            resources: Resources::new(512, 1, 1).unwrap(),
            network: NetworkPolicy::PublicInternet,
            secrets: Vec::new(),
            ports: Vec::new(),
            previous_disk: None,
        })
        .unwrap();
    let run = |command: &str| engine.exec(&name, &ExecRequest::new(["sh", "-c", command])).unwrap();
    assert_eq!(run("echo kept > /root/mark").code, 0);

    let check = |memory_mib: u32, cpus: u32, grow_disk_gib: Option<u32>| {
        engine.stop(&name).unwrap();
        engine.resize(&name, &ResizeSpec { memory_mib, cpus, grow_disk_gib }).unwrap();
        engine.start(&name).unwrap();
        let limit = engine.metrics(&name).unwrap().memory_limit_bytes;
        assert_eq!(limit, u64::from(memory_mib) << 20);
        assert_eq!(run("nproc").stdout.trim(), cpus.to_string());
        assert_eq!(run("cat /root/mark").stdout.trim(), "kept");
    };
    // Memory and CPUs alone, then the disk with them unchanged, then all down.
    check(1024, 2, None);
    check(1024, 2, Some(2));
    assert!(run("df -m / | tail -1 | awk '{print $2}'").stdout.trim().parse::<u32>().unwrap() > 1500);
    check(768, 1, None);

    // A disk of the same size is refused, and so is a smaller one.
    engine.stop(&name).unwrap();
    for disk in [2, 1] {
        assert!(engine.resize(&name, &ResizeSpec { memory_mib: 768, cpus: 1, grow_disk_gib: Some(disk) }).is_err());
    }
}
