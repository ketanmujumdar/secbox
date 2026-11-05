//! Hostile environment variables in the broker's own process must have no effect.
//! This is its own test binary because it changes process-wide environment, and
//! with one test there is nothing to race with.

mod common;

use std::env;
use std::fs;

use common::{commit_file, fixture_repo, make_bundle, plain_git, MB};
use gitbroker::{
    approve, ApprovalRequest, Approver, ApproverError, Decision, PushAuth, Quarantine,
};

struct Yes;
impl Approver for Yes {
    fn decide(&self, _: &ApprovalRequest<'_>) -> Result<Decision, ApproverError> {
        Ok(Decision::Approved {
            approver: "test".to_string(),
        })
    }
}

#[test]
fn hostile_environment_has_no_effect() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let src = fixture_repo(&root.join("src"));
    let tip = commit_file(&src, "a.txt", b"one\n", "first");
    let bundle = make_bundle(&src, &root.join("b.bundle"));
    let remote = root.join("remote.git");
    plain_git(&root, &["init", "-q", "--bare", remote.to_str().unwrap()]);

    let marker = root.join("marker");
    let trace = root.join("trace");
    let script = root.join("evil.sh");
    fs::write(&script, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
    std::process::Command::new("/bin/chmod")
        .args(["+x", script.to_str().unwrap()])
        .status()
        .unwrap();

    let evil = script.to_str().unwrap();
    let trace_path = trace.to_str().unwrap();
    env::set_var("GIT_DIR", root.join("nonexistent"));
    env::set_var("GIT_WORK_TREE", root.join("nonexistent"));
    env::set_var("GIT_OBJECT_DIRECTORY", root.join("nonexistent"));
    env::set_var("GIT_ALTERNATE_OBJECT_DIRECTORIES", root.join("nonexistent"));
    env::set_var("GIT_INDEX_FILE", root.join("nonexistent"));
    env::set_var("GIT_EXEC_PATH", root.join("nonexistent"));
    env::set_var("GIT_TEMPLATE_DIR", root.join("nonexistent"));
    env::set_var("GIT_SSH", evil);
    env::set_var("GIT_SSH_COMMAND", evil);
    env::set_var("GIT_EXTERNAL_DIFF", evil);
    env::set_var("GIT_PAGER", evil);
    env::set_var("PAGER", evil);
    env::set_var("GIT_ASKPASS", evil);
    env::set_var("SSH_ASKPASS", evil);
    env::set_var("GIT_EDITOR", evil);
    env::set_var("GIT_PROXY_COMMAND", evil);
    env::set_var("GIT_TRACE", trace_path);
    env::set_var("GIT_TRACE_PACKET", trace_path);
    env::set_var("GIT_CONFIG_COUNT", "1");
    env::set_var("GIT_CONFIG_KEY_0", "core.hooksPath");
    env::set_var("GIT_CONFIG_VALUE_0", root.join("nonexistent"));
    env::set_var("GIT_CONFIG_PARAMETERS", "'core.fsmonitor=/bin/false'");
    env::set_var("GIT_ALLOW_PROTOCOL", "ext:file:https");
    env::set_var("PATH", root.join("nonexistent"));

    let mut quarantine = Quarantine::create(&root.join("q.git")).unwrap();
    quarantine.import_bundle(&bundle, MB).unwrap();
    let review = quarantine.review(None, &tip).unwrap();
    let approval = approve(&Yes, &tip, &review).unwrap();
    let auth = PushAuth::none().allowing_local_remote();
    let receipt = quarantine
        .push(approval, remote.to_str().unwrap(), "scrub", &auth)
        .unwrap();
    assert_eq!(receipt.commit, tip);

    assert!(!marker.exists(), "a hostile helper ran");
    assert!(!trace.exists(), "GIT_TRACE was inherited");
}
