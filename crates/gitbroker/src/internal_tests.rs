//! Tests that need crate internals. An approval cannot be forged from outside
//! this crate, so tampering with one can only be simulated here.

use crate::approval::{approve, Approval, ApprovalRequest, Approver, ApproverError, Decision};
use crate::common::{commit_file, fixture_repo, make_bundle, plain_git, MB};
use crate::{Error, PushAuth, Quarantine};

struct Yes;
impl Approver for Yes {
    fn decide(&self, _: &ApprovalRequest<'_>) -> Result<Decision, ApproverError> {
        Ok(Decision::Approved {
            approver: "test".to_string(),
        })
    }
}

fn setup() -> (tempfile::TempDir, Quarantine, String, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let src = fixture_repo(&root.join("src"));
    let tip = commit_file(&src, "a.txt", b"one\n", "first");
    let bundle = make_bundle(&src, &root.join("b.bundle"));
    let mut quarantine = Quarantine::create(&root.join("q.git")).unwrap();
    quarantine.import_bundle(&bundle, MB).unwrap();
    let remote = root.join("remote.git");
    plain_git(&root, &["init", "-q", "--bare", remote.to_str().unwrap()]);
    (dir, quarantine, tip, remote)
}

#[test]
fn a_changed_tree_binding_stops_the_push() {
    let (_dir, quarantine, tip, remote) = setup();
    let review = quarantine.review(None, &tip).unwrap();
    let approval = approve(&Yes, &tip, &review).unwrap();
    let tampered = Approval {
        tree: "b".repeat(40),
        ..approval
    };
    let auth = PushAuth::none().allowing_local_remote();
    let err = quarantine
        .push(tampered, remote.to_str().unwrap(), "t", &auth)
        .unwrap_err();
    assert!(matches!(err, Error::TreeMismatch { .. }), "{err}");
    let refs = plain_git(&remote, &["for-each-ref"]);
    assert!(refs.is_empty(), "{refs}");
}

#[test]
fn an_approval_from_another_quarantine_stops_the_push() {
    let (dir, quarantine, tip, remote) = setup();
    let review = quarantine.review(None, &tip).unwrap();
    let approval = approve(&Yes, &tip, &review).unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let other = Quarantine::create(&root.join("other.git")).unwrap();
    let auth = PushAuth::none().allowing_local_remote();
    let err = other
        .push(approval, remote.to_str().unwrap(), "t", &auth)
        .unwrap_err();
    assert!(matches!(err, Error::WrongQuarantine), "{err}");
}
