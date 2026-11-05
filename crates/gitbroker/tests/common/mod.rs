//! Fixture helpers for tests. They create temporary repositories only, and they
//! run plain git with a scrubbed environment so the user's config is never read.

#![allow(dead_code)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const MB: u64 = 1024 * 1024;

fn fixture_command(cwd: &Path) -> Command {
    let mut cmd = Command::new("/usr/bin/git");
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .current_dir(cwd);
    cmd
}

/// Run ordinary, unhardened git. Output is trimmed. Panics on failure.
pub fn plain_git(cwd: &Path, args: &[&str]) -> String {
    let out = fixture_command(cwd).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Run plain git and return only whether it succeeded.
pub fn plain_git_status(cwd: &Path, args: &[&str]) -> bool {
    fixture_command(cwd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

pub fn plain_git_stdin(cwd: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = fixture_command(cwd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// A new repository on branch `main`.
pub fn fixture_repo(path: &Path) -> PathBuf {
    fs::create_dir_all(path).unwrap();
    plain_git(path, &["init", "-q", "--initial-branch=main"]);
    path.to_path_buf()
}

/// Write a file, commit everything, and return the commit id.
pub fn commit_file(repo: &Path, path: &str, content: &[u8], message: &str) -> String {
    let full = repo.join(path);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(full, content).unwrap();
    commit_all(repo, message)
}

pub fn commit_all(repo: &Path, message: &str) -> String {
    plain_git(repo, &["add", "-A"]);
    plain_git(repo, &["commit", "-q", "--allow-empty", "-m", message]);
    plain_git(repo, &["rev-parse", "HEAD"])
}

/// A full bundle of every branch.
pub fn make_bundle(repo: &Path, dest: &Path) -> PathBuf {
    plain_git(
        repo,
        &[
            "bundle",
            "create",
            "-q",
            dest.to_str().unwrap(),
            "--branches",
        ],
    );
    dest.to_path_buf()
}

/// A bundle with a handwritten header, as a hostile VM could write it.
pub fn raw_bundle(dest: &Path, ref_lines: &[String], pack: &[u8]) -> PathBuf {
    let mut data = b"# v2 git bundle\n".to_vec();
    for line in ref_lines {
        data.extend_from_slice(line.as_bytes());
        data.push(b'\n');
    }
    data.push(b'\n');
    data.extend_from_slice(pack);
    fs::write(dest, data).unwrap();
    dest.to_path_buf()
}

/// A pack holding exactly the named objects.
pub fn pack_objects(repo: &Path, oids: &[&str]) -> Vec<u8> {
    let list = oids.join("\n") + "\n";
    plain_git_stdin(repo, &["pack-objects", "--stdout", "-q"], list.as_bytes())
}

/// A pack holding everything reachable from `tip`.
pub fn pack_reachable(repo: &Path, tip: &str) -> Vec<u8> {
    let list = format!("{tip}\n");
    plain_git_stdin(
        repo,
        &["pack-objects", "--revs", "--stdout", "-q"],
        list.as_bytes(),
    )
}

pub fn hex_to_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

/// Store a tree object with one raw entry, bypassing git's own checks, and return
/// a commit that points at it, with the tree and blob ids.
pub fn malformed_commit(repo: &Path, mode: &str, name: &str) -> (String, String, String) {
    let blob = String::from_utf8(plain_git_stdin(
        repo,
        &["hash-object", "-w", "--stdin"],
        b"payload\n",
    ))
    .unwrap()
    .trim()
    .to_string();
    let mut tree = format!("{mode} {name}\0").into_bytes();
    tree.extend(hex_to_bytes(&blob));
    let tree_id = String::from_utf8(plain_git_stdin(
        repo,
        &["hash-object", "-t", "tree", "--literally", "-w", "--stdin"],
        &tree,
    ))
    .unwrap()
    .trim()
    .to_string();
    let commit = format!(
        "tree {tree_id}\nauthor a <a@example.invalid> 1 +0000\ncommitter a <a@example.invalid> 1 +0000\n\nmalformed\n"
    );
    let commit_id = String::from_utf8(plain_git_stdin(
        repo,
        &[
            "hash-object",
            "-t",
            "commit",
            "--literally",
            "-w",
            "--stdin",
        ],
        commit.as_bytes(),
    ))
    .unwrap()
    .trim()
    .to_string();
    (commit_id, tree_id, blob)
}
