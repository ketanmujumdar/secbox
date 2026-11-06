//! A commit of exactly what a VM was given, made on the host, so that what
//! the VM sends back can be compared with it.
//!
//! A snapshot is a plain directory with no history, because secret files were
//! left out of it and the user's own `.git` must never reach the VM. To review
//! what changed inside the VM, both sides need a commit of that directory:
//!
//! ```text
//! make_baseline(snapshot, store)
//!   snapshot/.git   a new repository with one commit of the snapshot; it is
//!                   copied into the VM with the files, as the VM's history
//!   store           a bare repository that keeps the same commit on the host
//! ```
//!
//! Both are made here, by the host, before the VM sees either. The commit id
//! is returned, and the caller keeps it: a later bundle from the VM is reviewed
//! against that id and the store, never against anything the VM says its
//! starting point was. A SHA-1 id cannot be matched by different content in
//! practice, and git's collision detection catches the known attack.
//!
//! The snapshot directory is the host's own staging copy of the user's
//! project. Its contents are still hostile data, so git runs with the same
//! hardened options as everywhere else, and the project's `.gitignore` is
//! overridden so the commit holds every file the VM receives.

use std::fs;
use std::path::Path;

use crate::display::escape_for_display;
use crate::error::{Error, Result};
use crate::git::GitCmd;
use crate::quarantine::Quarantine;
use crate::validate::validate_commit_id;

/// The branch the snapshot's repository starts on.
pub const BASELINE_BRANCH: &str = "main";
const MESSAGE: &str = "Snapshot given to the sandbox\n";
const IDENT_NAME: &str = "snapshot";
const IDENT_EMAIL: &str = "snapshot@localhost.invalid";

/// Make `snapshot` a repository holding one commit of everything in it, and
/// keep that commit in a new bare repository at `store`. Returns the commit id.
///
/// `snapshot` must be an absolute path to a directory with no `.git` in it.
/// `store` must not exist yet.
pub fn make_baseline(snapshot: &Path, store: &Path) -> Result<String> {
    let shown = || escape_for_display(snapshot.as_os_str().as_encoded_bytes());
    if !snapshot.is_absolute() {
        return Err(Error::NotAQuarantine(shown()));
    }
    let meta = fs::symlink_metadata(snapshot)?;
    if !meta.is_dir() {
        return Err(Error::NotAQuarantine(shown()));
    }
    let dot_git = snapshot.join(".git");
    if fs::symlink_metadata(&dot_git).is_ok() {
        return Err(Error::PathExists(escape_for_display(
            dot_git.as_os_str().as_encoded_bytes(),
        )));
    }
    if fs::symlink_metadata(store).is_ok() {
        return Err(Error::PathExists(escape_for_display(
            store.as_os_str().as_encoded_bytes(),
        )));
    }

    GitCmd::new("init")
        .args([
            "init",
            "--quiet",
            "--template=",
            "--object-format=sha1",
            &format!("--initial-branch={BASELINE_BRANCH}"),
            "--",
        ])
        .arg(snapshot)
        .ok()?;

    let mut work_tree = std::ffi::OsString::from("--work-tree=");
    work_tree.push(snapshot);
    // Everything the VM is given, whatever the project's ignore files say.
    GitCmd::new("add")
        .git_dir(&dot_git)
        .arg(&work_tree)
        .args(["add", "--all", "--force", "--"])
        .arg(snapshot)
        .timeout(std::time::Duration::from_secs(600))
        .ok()?;
    let tree = GitCmd::new("write-tree")
        .git_dir(&dot_git)
        .args(["write-tree"])
        .ok()?
        .stdout_trimmed();
    validate_commit_id(&tree)?;
    let commit = GitCmd::new("commit-tree")
        .git_dir(&dot_git)
        .config(format!("user.name={IDENT_NAME}"))
        .config(format!("user.email={IDENT_EMAIL}"))
        .config("commit.gpgSign=false")
        .args(["commit-tree", "-m", MESSAGE, &tree])
        .ok()?
        .stdout_trimmed();
    validate_commit_id(&commit)?;
    GitCmd::new("update-ref")
        .git_dir(&dot_git)
        .args([
            "update-ref",
            &format!("refs/heads/{BASELINE_BRANCH}"),
            &commit,
        ])
        .ok()?;

    let mut kept = Quarantine::create(store)?;
    kept.import_base(snapshot, &commit)?;
    Ok(commit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::plain_git;

    fn root() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        (dir, root)
    }

    #[test]
    fn the_commit_holds_every_file_even_ignored_ones() {
        let (_dir, root) = root();
        let snap = root.join("snap");
        fs::create_dir_all(snap.join("sub")).unwrap();
        fs::write(snap.join("a.txt"), "a\n").unwrap();
        fs::write(snap.join("sub/b.txt"), "b\n").unwrap();
        fs::write(snap.join(".gitignore"), "sub/\n*.txt\n").unwrap();
        let commit = make_baseline(&snap, &root.join("store.git")).unwrap();

        let files = plain_git(&snap, &["ls-tree", "-r", "--name-only", &commit]);
        let files: Vec<&str> = files.lines().collect();
        assert_eq!(files, [".gitignore", "a.txt", "sub/b.txt"]);
        assert_eq!(plain_git(&snap, &["rev-parse", "HEAD"]), commit);
        // The store has the same commit, under the base prefix only.
        let kept = plain_git(
            &root,
            &[
                "--git-dir",
                root.join("store.git").to_str().unwrap(),
                "for-each-ref",
                "--format=%(refname) %(objectname)",
            ],
        );
        assert_eq!(kept, format!("refs/base/{commit} {commit}"));
    }

    #[test]
    fn an_existing_git_dir_or_store_is_refused() {
        let (_dir, root) = root();
        let snap = root.join("snap");
        fs::create_dir_all(snap.join(".git")).unwrap();
        assert!(matches!(
            make_baseline(&snap, &root.join("store.git")),
            Err(Error::PathExists(_))
        ));
        fs::remove_dir(snap.join(".git")).unwrap();
        fs::create_dir(root.join("store.git")).unwrap();
        assert!(matches!(
            make_baseline(&snap, &root.join("store.git")),
            Err(Error::PathExists(_))
        ));
    }

    #[test]
    fn a_relative_or_missing_snapshot_is_refused() {
        let (_dir, root) = root();
        assert!(make_baseline(Path::new("rel"), &root.join("s.git")).is_err());
        assert!(make_baseline(&root.join("missing"), &root.join("s.git")).is_err());
    }

    /// A hook-shaped file in the project is only a file: nothing in the
    /// snapshot can make the host run anything while the commit is made.
    #[test]
    fn nothing_in_the_snapshot_runs() {
        let (_dir, root) = root();
        let snap = root.join("snap");
        let marker = root.join("ran");
        fs::create_dir_all(snap.join(".githooks")).unwrap();
        let script = format!("#!/bin/sh\ntouch {}\n", marker.display());
        fs::write(snap.join(".githooks/pre-commit"), &script).unwrap();
        fs::write(snap.join(".gitattributes"), "* filter=evil diff=evil\n").unwrap();
        make_baseline(&snap, &root.join("store.git")).unwrap();
        assert!(!marker.exists());
    }
}
