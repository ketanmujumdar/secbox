//! Export a commit from a trusted repository into a directory for the VM.
//!
//! The archive is read by this crate, and excluded files are never written to the
//! destination. A final walk then checks the result on disk, so the guarantee
//! does not rest on the reader alone. Symlinks are created last, and each must
//! resolve inside the destination, so nothing can be written through one.
//!
//! Two layers keep secrets out. Names are matched against the rules in
//! [`crate::secretpath`], and anything that matches is never written; the
//! report says what went and why. Everything that is written is also read as it
//! is copied, and a file that looks like it holds a secret is reported as a
//! warning. A warning is for a person to judge: the name rules cannot know that
//! an ordinary source file has a key pasted into it, and refusing to copy it
//! would make the snapshot useless.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, DirBuilderExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::detect::{high_entropy_value, scan_line, SecretRule};
use crate::display::{escape_for_display, fold_component};
use crate::error::{Error, Result};
use crate::git::GitCmd;
use crate::secretpath::{path_exclusion, ExclusionReason};
use crate::tar::{self, Entry, EntryKind};
use crate::validate::validate_commit_id;

const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_DEPTH: usize = 256;

/// Bounds on the content scan. The scan is a warning aid, so going past any of
/// them only clears [`SnapshotReport::scan_complete`]; the copy itself finishes.
const MAX_SCAN_BYTES_PER_FILE: u64 = 1024 * 1024;
const MAX_SCAN_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const MAX_WARNINGS: usize = 1000;
/// A line longer than this is checked in pieces, so one line without a newline
/// cannot hold the whole file in memory. Each piece keeps the last
/// [`LINE_OVERLAP`] bytes of the one before, which is longer than any shape the
/// detector looks for, so a token on the seam is still seen.
const MAX_LINE_BYTES: usize = 64 * 1024;
const LINE_OVERLAP: usize = 256;
const COPY_CHUNK: usize = 64 * 1024;

/// One entry left out of the snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExcludedEntry {
    /// Escaped and safe to print.
    pub path: String,
    pub reason: ExclusionReason,
}

/// A file that was copied although something in it looks like a secret.
///
/// This is a warning, not an exclusion: the file is in the snapshot. It holds
/// no part of the suspected value, only where to look and which rule matched.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ContentWarning {
    /// Escaped and safe to print.
    pub path: String,
    pub rule: SecretRule,
    /// One-based line where the rule first matched.
    pub line: u64,
}

/// What the snapshot contains, what it left out, and what it warns about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SnapshotReport {
    pub commit: String,
    pub files_written: u64,
    pub symlinks_written: u64,
    /// Everything left out, with the reason. Directories are not listed: their
    /// files say the same thing.
    pub excluded: Vec<ExcludedEntry>,
    /// Likely secrets inside files that were copied. One entry per file and
    /// rule, in the order the files were read.
    pub warnings: Vec<ContentWarning>,
    /// False when a scan limit was reached, so the warnings understate the risk.
    /// Exclusions are unaffected: they are decided from names alone.
    pub scan_complete: bool,
}

/// Resolve the git directory of a trusted repository without discovery, so a
/// path inside some other repository cannot silently select that repository.
pub(crate) fn git_dir_of(repo: &Path) -> Result<PathBuf> {
    let not_repo = || Error::NotAQuarantine(escape_for_display(repo.as_os_str().as_bytes()));
    if !repo.is_absolute() {
        return Err(not_repo());
    }
    let dot_git = repo.join(".git");
    let meta = std::fs::symlink_metadata(&dot_git);
    if meta.as_ref().is_ok_and(|m| m.is_dir()) {
        Ok(dot_git)
    } else if meta.as_ref().is_ok_and(|m| m.is_file()) {
        // A `.git` file points at the repository: written by `git worktree`,
        // and by the daemon for a project fetched from a remote. It is read
        // here rather than left to git, so the folder git is given is known.
        gitfile_target(repo, &dot_git).ok_or_else(not_repo)
    } else if repo.join("HEAD").is_file() && repo.join("objects").is_dir() {
        Ok(repo.to_path_buf())
    } else {
        Err(not_repo())
    }
}

/// Follows a `gitdir:` line to a folder that looks like a repository.
fn gitfile_target(repo: &Path, file: &Path) -> Option<PathBuf> {
    use std::io::Read as _;
    let mut text = String::new();
    File::open(file).ok()?.take(4096).read_to_string(&mut text).ok()?;
    let target = text.lines().find_map(|line| line.strip_prefix("gitdir:"))?.trim();
    if target.is_empty() {
        return None;
    }
    let target = Path::new(target);
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        repo.join(target)
    };
    let real = std::fs::canonicalize(target).ok()?;
    (real.join("HEAD").is_file()).then_some(real)
}

/// Split into checked components. `.`, `..`, empty parts and absolute paths mean
/// the archive is hostile or damaged, so they abort the snapshot.
fn checked_components(path: &[u8]) -> Result<Vec<&[u8]>> {
    let bad = || Error::PathTraversal(escape_for_display(path));
    if path.is_empty() || path[0] == b'/' {
        return Err(bad());
    }
    let parts: Vec<&[u8]> = path.split(|b| *b == b'/').collect();
    if parts.len() > MAX_DEPTH {
        return Err(bad());
    }
    for part in &parts {
        if part.is_empty() || *part == b"." || *part == b".." {
            return Err(bad());
        }
        // Lookups go through the filesystem's folding, so a folded `..` is out too.
        let folded = fold_component(part);
        if folded == ".." || folded == "." {
            return Err(bad());
        }
    }
    Ok(parts)
}

/// Create the parents of `parts` under `root`, refusing to pass through anything
/// that is not a real directory.
fn prepare_parents(root: &Path, parts: &[&[u8]]) -> Result<PathBuf> {
    let mut current = root.to_path_buf();
    for part in parts {
        current.push(OsStr::from_bytes(part));
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(Error::SnapshotCollision(escape_for_display(
                    current.as_os_str().as_bytes(),
                )));
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {
                DirBuilder::new().mode(0o755).create(&current)?;
            }
            Err(err) => return Err(Error::Io(err)),
        }
    }
    Ok(current)
}

/// Reads a file's bytes as they are copied and remembers which rules matched.
///
/// It keeps one line at a time, never the match itself, and stops once a limit
/// is reached so a large repository cannot make the snapshot expensive.
#[derive(Default)]
struct ContentScanner {
    pending: Vec<u8>,
    line: u64,
    /// Rule to the first line it matched on. One entry per rule keeps a file of
    /// ten thousand keys from filling the report.
    hits: BTreeMap<SecretRule, u64>,
    scanned: u64,
    /// Set when a limit stopped the scan of this file.
    stopped: bool,
}

impl ContentScanner {
    /// Scan what fits of `chunk`, taking from the snapshot-wide `budget`.
    fn feed(&mut self, chunk: &[u8], budget: &mut u64) {
        if self.stopped {
            return;
        }
        let room = MAX_SCAN_BYTES_PER_FILE.saturating_sub(self.scanned);
        let allowed = room.min(*budget).min(chunk.len() as u64) as usize;
        if allowed < chunk.len() {
            self.stopped = true;
        }
        self.scanned += allowed as u64;
        *budget -= allowed as u64;
        self.absorb(&chunk[..allowed]);
    }

    fn absorb(&mut self, data: &[u8]) {
        let mut rest = data;
        while let Some(at) = rest.iter().position(|b| *b == b'\n') {
            let (head, tail) = rest.split_at(at);
            if self.pending.is_empty() {
                self.check(head);
            } else {
                let mut line = std::mem::take(&mut self.pending);
                line.extend_from_slice(head);
                self.check(&line);
            }
            self.line += 1;
            rest = &tail[1..];
        }
        self.pending.extend_from_slice(rest);
        if self.pending.len() > MAX_LINE_BYTES {
            let line = std::mem::take(&mut self.pending);
            self.check(&line);
            self.pending
                .extend_from_slice(&line[line.len() - LINE_OVERLAP..]);
        }
    }

    fn check(&mut self, line: &[u8]) {
        let at = self.line + 1;
        for rule in scan_line(line) {
            self.hits.entry(rule).or_insert(at);
        }
        if high_entropy_value(line) {
            self.hits.entry(SecretRule::HighEntropy).or_insert(at);
        }
    }

    /// The findings, and whether the whole file was seen.
    fn finish(mut self) -> (Vec<(SecretRule, u64)>, bool) {
        let tail = std::mem::take(&mut self.pending);
        if !tail.is_empty() {
            self.check(&tail);
        }
        (self.hits.into_iter().collect(), !self.stopped)
    }
}

/// Copy `body` into `file`, scanning as it goes. Returns the bytes copied.
fn copy_scanning(
    body: &mut dyn Read,
    file: &mut File,
    scanner: &mut ContentScanner,
    budget: &mut u64,
) -> io::Result<u64> {
    let mut buffer = vec![0u8; COPY_CHUNK];
    let mut copied = 0u64;
    loop {
        let read = match body.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        file.write_all(&buffer[..read])?;
        copied += read as u64;
        scanner.feed(&buffer[..read], budget);
    }
    Ok(copied)
}

struct Extraction<'a> {
    root: &'a Path,
    report: SnapshotReport,
    deferred_links: Vec<(Vec<u8>, Vec<u8>)>,
    /// Bytes still available to the content scan, across all files.
    scan_budget: u64,
}

impl Extraction<'_> {
    fn exclude(&mut self, path: &[u8], reason: ExclusionReason) {
        self.report.excluded.push(ExcludedEntry {
            path: safe_display(path),
            reason,
        });
    }

    fn warn(&mut self, path: &[u8], hits: Vec<(SecretRule, u64)>) {
        for (rule, line) in hits {
            if self.report.warnings.len() >= MAX_WARNINGS {
                self.report.scan_complete = false;
                return;
            }
            self.report.warnings.push(ContentWarning {
                path: safe_display(path),
                rule,
                line,
            });
        }
    }

    fn accept(&mut self, entry: Entry, body: &mut dyn Read) -> Result<()> {
        let parts = checked_components(&entry.path)?;
        let is_file = matches!(entry.kind, EntryKind::File { .. });
        if let Some(reason) = path_exclusion(&parts, is_file) {
            // Reporting directories too would only repeat what their files say.
            if !matches!(entry.kind, EntryKind::Directory) {
                self.exclude(&entry.path, reason);
            }
            return Ok(());
        }
        match &entry.kind {
            EntryKind::Directory => {
                prepare_parents(self.root, &parts)?;
            }
            EntryKind::File { executable } => {
                let (name, parents) = parts
                    .split_last()
                    .ok_or_else(|| Error::PathTraversal(escape_for_display(&entry.path)))?;
                let mut target = prepare_parents(self.root, parents)?;
                target.push(OsStr::from_bytes(name));
                let mode = if *executable { 0o755 } else { 0o644 };
                // create_new will not follow or overwrite anything already there,
                // which also catches names that collide under case folding.
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(mode)
                    .open(&target)
                    .map_err(|err| collision_or_io(err, &entry.path))?;
                let mut scanner = ContentScanner::default();
                let copied = copy_scanning(body, &mut file, &mut scanner, &mut self.scan_budget)?;
                if copied != entry.size {
                    return Err(Error::SnapshotFormat("archive is truncated"));
                }
                let (hits, whole_file) = scanner.finish();
                if !whole_file {
                    self.report.scan_complete = false;
                }
                self.warn(&entry.path, hits);
                self.report.files_written += 1;
            }
            EntryKind::Symlink { target } => {
                self.deferred_links
                    .push((entry.path.clone(), target.clone()));
            }
        }
        Ok(())
    }

    fn create_links(&mut self) -> Result<Vec<(Vec<u8>, PathBuf)>> {
        let mut created = Vec::new();
        for (path, target) in std::mem::take(&mut self.deferred_links) {
            let target_path = Path::new(OsStr::from_bytes(&target));
            let parts = checked_components(&path)?;
            let (name, parents) = parts
                .split_last()
                .ok_or_else(|| Error::PathTraversal(escape_for_display(&path)))?;
            if target.is_empty()
                || target_path.is_absolute()
                || escapes_lexically(&parts, target_path)
            {
                self.exclude(&path, ExclusionReason::EscapingSymlink);
                continue;
            }
            let mut location = prepare_parents(self.root, parents)?;
            location.push(OsStr::from_bytes(name));
            symlink(target_path, &location).map_err(|err| collision_or_io(err, &path))?;
            created.push((path, location));
            self.report.symlinks_written += 1;
        }
        Ok(created)
    }

    /// Lexical checks can be fooled when one link passes through another, so check
    /// what each link really resolves to. Removing one can break another, so
    /// repeat until nothing changes.
    fn remove_escaping_links(&mut self, mut links: Vec<(Vec<u8>, PathBuf)>) -> Result<()> {
        let root = fs::canonicalize(self.root)?;
        loop {
            let mut kept = Vec::with_capacity(links.len());
            let mut removed_any = false;
            for (path, location) in links {
                let inside = fs::canonicalize(&location)
                    .map(|real| real.starts_with(&root))
                    .unwrap_or(false);
                if inside {
                    kept.push((path, location));
                } else {
                    match fs::remove_file(&location) {
                        Ok(()) => {}
                        Err(err) if err.kind() == ErrorKind::NotFound => {}
                        Err(err) => return Err(Error::Io(err)),
                    }
                    self.exclude(&path, ExclusionReason::EscapingSymlink);
                    self.report.symlinks_written -= 1;
                    removed_any = true;
                }
            }
            links = kept;
            if !removed_any {
                return Ok(());
            }
        }
    }

    /// The guarantee: walk what is really on disk, without following links, and
    /// remove anything that should not be there. The names on disk are judged,
    /// not the names in the archive, so a filesystem that folded two names
    /// together cannot leave a secret behind.
    fn sweep(&mut self) -> Result<()> {
        let mut pending = vec![(self.root.to_path_buf(), Vec::<Vec<u8>>::new())];
        while let Some((dir, parents)) = pending.pop() {
            for item in fs::read_dir(&dir)? {
                let item = item?;
                let path = item.path();
                let name = item.file_name();
                let meta = fs::symlink_metadata(&path)?;
                let mut parts = parents.clone();
                parts.push(name.as_bytes().to_vec());
                let borrowed: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
                if let Some(reason) = path_exclusion(&borrowed, meta.is_file()) {
                    if meta.is_dir() {
                        fs::remove_dir_all(&path)?;
                    } else {
                        fs::remove_file(&path)?;
                    }
                    let shown = path.strip_prefix(self.root).unwrap_or(&path);
                    self.exclude(shown.as_os_str().as_bytes(), reason);
                } else if meta.is_dir() {
                    pending.push((path, parts));
                }
            }
        }
        Ok(())
    }
}

/// A path is hostile data and can itself be a secret, so a name that trips the
/// detector is withheld instead of printed.
fn safe_display(path: &[u8]) -> String {
    if scan_line(path).is_empty() {
        escape_for_display(path)
    } else {
        "\"(path withheld: the name itself looks like a secret)\"".to_string()
    }
}

fn collision_or_io(err: io::Error, path: &[u8]) -> Error {
    if err.kind() == ErrorKind::AlreadyExists {
        Error::SnapshotCollision(escape_for_display(path))
    } else {
        Error::Io(err)
    }
}

/// Whether a relative link target climbs above the snapshot root, judged from the
/// directory holding the link. This is the fast check. The real one follows.
fn escapes_lexically(parts: &[&[u8]], target: &Path) -> bool {
    let mut depth = parts.len() as i64 - 1;
    for component in target.components() {
        match component {
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => return true,
        }
    }
    false
}

fn extract(archive: &Path, root: &Path, commit: &str) -> Result<SnapshotReport> {
    let mut extraction = Extraction {
        root,
        report: SnapshotReport {
            commit: commit.to_string(),
            files_written: 0,
            symlinks_written: 0,
            excluded: Vec::new(),
            warnings: Vec::new(),
            scan_complete: true,
        },
        deferred_links: Vec::new(),
        scan_budget: MAX_SCAN_TOTAL_BYTES,
    };
    tar::walk(File::open(archive)?, |entry, body| {
        extraction.accept(entry, body)
    })?;
    let links = extraction.create_links()?;
    extraction.sweep()?;
    extraction.remove_escaping_links(links)?;
    Ok(extraction.report)
}

/// Export `commit` from `trusted_repo` into `dest`, which must not exist yet.
///
/// Common secret file names are left out in any letter case and in any
/// directory, and each one is listed in [`SnapshotReport::excluded`] with its
/// [`ExclusionReason`]. The exception is a plain example file of the `.env` or
/// Terraform variable families, such as `.env.example`. Every file that is
/// copied is also read for likely secrets, which are reported as
/// [`SnapshotReport::warnings`] without the suspected value.
///
/// Any failure removes `dest`, so a partial snapshot never stays.
///
/// `trusted_repo` is the host's own repository and must never be one the VM wrote.
/// Hardened config still overrides hooks and helpers in its config.
pub fn snapshot(trusted_repo: &Path, commit: &str, dest: &Path) -> Result<SnapshotReport> {
    validate_commit_id(commit)?;
    let git_dir = git_dir_of(trusted_repo)?;
    if !dest.is_absolute() {
        return Err(Error::PathTraversal(escape_for_display(
            dest.as_os_str().as_bytes(),
        )));
    }

    // Holds the whole archive, including files that will be excluded, until this
    // function returns. It is private to this user and removed on drop.
    let work = tempfile::Builder::new().prefix("gb-snapshot-").tempdir()?;
    let archive = work.path().join("archive.tar");
    GitCmd::new("archive")
        .git_dir(&git_dir)
        .args(["archive", "--format=tar", commit])
        .stdout_file(&archive)
        .max_stdout(MAX_ARCHIVE_BYTES)
        .timeout(ARCHIVE_TIMEOUT)
        .ok()?;

    DirBuilder::new().mode(0o700).create(dest).map_err(|err| {
        if err.kind() == ErrorKind::AlreadyExists {
            Error::PathExists(escape_for_display(dest.as_os_str().as_bytes()))
        } else {
            Error::Io(err)
        }
    })?;
    match extract(&archive, dest, commit) {
        Ok(report) => Ok(report),
        Err(err) => {
            let _ = fs::remove_dir_all(dest);
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tar::tests::{file_entry, header};

    fn run_extract(data: Vec<u8>) -> (tempfile::TempDir, PathBuf, Result<SnapshotReport>) {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar");
        let mut data = data;
        data.extend(vec![0u8; 1024]);
        fs::write(&archive, data).unwrap();
        let root = fs::canonicalize(dir.path()).unwrap().join("dest");
        fs::create_dir(&root).unwrap();
        let result = extract(&archive, &root, "c");
        (dir, root, result)
    }

    #[test]
    fn traversal_entries_abort() {
        for path in [
            &b"../evil"[..],
            b"a/../../evil",
            b"/etc/evil",
            b"a//b",
            b"./a",
        ] {
            let (_dir, root, result) = run_extract(file_entry(path, b"x"));
            assert!(matches!(result, Err(Error::PathTraversal(_))), "{path:?}");
            assert!(!root.parent().unwrap().join("evil").exists());
        }
    }

    #[test]
    fn env_is_never_written_and_is_reported() {
        let mut data = file_entry(b".env", b"S");
        data.extend(file_entry(b"sub/.ENV.local", b"S"));
        data.extend(file_entry(b".env.example", b"ok"));
        data.extend(file_entry(b"sub/.env.example", b"ok"));
        data.extend(file_entry(b".env/inner", b"S"));
        data.extend(file_entry(b".env.example/inner", b"S"));
        data.extend(file_entry(b"sub/.E\xe2\x80\x8cNV", b"S"));
        data.extend(file_entry(b"keep.txt", b"ok"));
        let (_dir, root, result) = run_extract(data);
        let report = result.unwrap();
        assert_eq!(report.files_written, 3);
        assert_eq!(report.excluded.len(), 5, "{:?}", report.excluded);
        assert!(root.join(".env.example").is_file());
        assert!(root.join("sub/.env.example").is_file());
        assert!(root.join("keep.txt").is_file());
        assert!(!root.join(".env").exists());
        assert!(!root.join("sub/.ENV.local").exists());
    }

    #[test]
    fn a_symlink_cannot_stand_in_for_env_example() {
        let data = header(b".env.example", b'2', 0, b"keep").to_vec();
        let (_dir, root, result) = run_extract(data);
        let report = result.unwrap();
        assert_eq!(report.excluded[0].reason, ExclusionReason::EnvFile);
        assert!(fs::symlink_metadata(root.join(".env.example")).is_err());
    }

    #[test]
    fn common_secret_files_are_never_written_and_are_reported() {
        let table: &[(&[u8], ExclusionReason)] = &[
            (b"deploy/id_rsa", ExclusionReason::PrivateKey),
            (b"certs/server.pem", ExclusionReason::PrivateKey),
            (b"android/release.keystore", ExclusionReason::PrivateKey),
            (b".ssh/known_hosts", ExclusionReason::PrivateKey),
            (b".aws/credentials", ExclusionReason::CloudCredentials),
            (b"ops/dev.kubeconfig", ExclusionReason::CloudCredentials),
            (b".npmrc", ExclusionReason::ToolCredentials),
            (b".docker/config.json", ExclusionReason::ToolCredentials),
            (b".git-credentials", ExclusionReason::ToolCredentials),
            (b"infra/terraform.tfvars", ExclusionReason::InfrastructureState),
            (b"infra/terraform.tfstate", ExclusionReason::InfrastructureState),
            (b"vault.kdbx", ExclusionReason::SecretStore),
            (b"k8s/secrets.yaml", ExclusionReason::SecretStore),
            (b".cursor/mcp.json", ExclusionReason::EditorOrAgentSecret),
            (b".vscode/sftp.json", ExclusionReason::EditorOrAgentSecret),
        ];
        let mut data = Vec::new();
        for (path, _) in table {
            data.extend(file_entry(path, b"S"));
        }
        data.extend(file_entry(b".env.example", b"TOKEN=placeholder"));
        data.extend(file_entry(b"infra/terraform.tfvars.example", b"a = 1"));
        data.extend(file_entry(b"src/main.rs", b"fn main() {}"));
        let (_dir, root, result) = run_extract(data);
        let report = result.unwrap();

        assert_eq!(report.files_written, 3, "{:?}", report.excluded);
        assert_eq!(report.excluded.len(), table.len());
        for (path, reason) in table {
            let shown = escape_for_display(path);
            let found = report.excluded.iter().find(|e| e.path == shown);
            assert_eq!(found.map(|e| e.reason), Some(*reason), "{shown}");
            let on_disk = root.join(OsStr::from_bytes(path));
            assert!(fs::symlink_metadata(on_disk).is_err(), "{shown}");
        }
        assert!(root.join(".env.example").is_file());
        assert!(root.join("infra/terraform.tfvars.example").is_file());
        assert!(root.join("src/main.rs").is_file());
    }

    #[test]
    fn a_secret_in_a_copied_file_is_warned_about_without_the_value() {
        let token = format!("ghp_{}", "a".repeat(36));
        let body = format!(
            "import os\nTOKEN = \"{token}\"\napi_key = \"Xq7PzR2mWv9TkLbN4sYc8H\"\n"
        );
        let mut data = file_entry(b"app/config.py", body.as_bytes());
        data.extend(file_entry(b"README.md", b"nothing to see\n"));
        let (_dir, root, result) = run_extract(data);
        let report = result.unwrap();

        // The file is copied: this is a warning, not an exclusion.
        assert!(root.join("app/config.py").is_file());
        assert!(report.excluded.is_empty());
        assert!(report.scan_complete);
        assert_eq!(report.warnings.len(), 2, "{:?}", report.warnings);
        let github = report
            .warnings
            .iter()
            .find(|w| w.rule == SecretRule::GithubToken)
            .unwrap();
        assert_eq!(github.path, r#""app/config.py""#);
        assert_eq!(github.line, 2);
        let entropy = report
            .warnings
            .iter()
            .find(|w| w.rule == SecretRule::HighEntropy)
            .unwrap();
        assert_eq!(entropy.line, 3);
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains(&token), "{json}");
        assert!(!json.contains("Xq7PzR2"), "{json}");
    }

    #[test]
    fn one_warning_per_file_and_rule() {
        let key = format!("AKIA{}", "IOSFODNN7EXAMPLE");
        let body = format!("one = {key}\ntwo = {key}\n");
        let (_dir, _root, result) = run_extract(file_entry(b"a.tf", body.as_bytes()));
        let report = result.unwrap();
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].rule, SecretRule::AwsAccessKey);
        assert_eq!(report.warnings[0].line, 1);
    }

    #[test]
    fn a_file_too_large_to_scan_clears_scan_complete() {
        let key = format!("AKIA{}", "IOSFODNN7EXAMPLE");
        let mut body = vec![b'x'; MAX_SCAN_BYTES_PER_FILE as usize + 16];
        body.extend_from_slice(format!("\n{key}\n").as_bytes());
        let (_dir, root, result) = run_extract(file_entry(b"big.bin", &body));
        let report = result.unwrap();
        assert!(root.join("big.bin").is_file(), "the file is still copied");
        assert!(!report.scan_complete);
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn a_name_that_is_itself_a_secret_is_withheld_from_the_report() {
        let name = format!("ghp_{}", "b".repeat(36));
        let data = file_entry(name.as_bytes(), b"-----BEGIN PRIVATE KEY-----\n");
        let (_dir, _root, result) = run_extract(data);
        let report = result.unwrap();
        assert_eq!(report.warnings.len(), 1);
        assert!(!report.warnings[0].path.contains("ghp_"));
        assert_eq!(report.warnings[0].rule, SecretRule::PrivateKey);
    }

    #[test]
    fn git_metadata_is_excluded() {
        let mut data = file_entry(b".git/config", b"x");
        data.extend(file_entry(b"sub/.GIT/hooks/pre-push", b"x"));
        let (_dir, root, result) = run_extract(data);
        let report = result.unwrap();
        assert_eq!(report.excluded.len(), 2);
        assert!(report
            .excluded
            .iter()
            .all(|e| e.reason == ExclusionReason::GitMetadata));
        assert!(!root.join(".git").exists());
    }

    #[test]
    fn escaping_symlinks_are_removed_and_inner_ones_kept() {
        let mut data = file_entry(b"real.txt", b"x");
        data.extend_from_slice(&header(b"good", b'2', 0, b"real.txt"));
        data.extend_from_slice(&header(b"abs", b'2', 0, b"/etc/passwd"));
        data.extend_from_slice(&header(b"up", b'2', 0, b"../outside"));
        data.extend_from_slice(&header(b"dangling", b'2', 0, b"missing"));
        data.extend_from_slice(&header(b"to-dangling", b'2', 0, b"dangling"));
        let (_dir, root, result) = run_extract(data);
        let report = result.unwrap();
        assert!(fs::symlink_metadata(root.join("good")).is_ok());
        for gone in ["abs", "up", "dangling", "to-dangling"] {
            assert!(fs::symlink_metadata(root.join(gone)).is_err(), "{gone}");
        }
        assert_eq!(report.symlinks_written, 1);
        assert_eq!(report.excluded.len(), 4);
    }

    #[test]
    fn a_chain_that_passes_lexical_checks_is_still_caught() {
        // x/y/l resolves to the root, so m climbs above it, yet "x/y/l/../.." looks
        // safe when read as plain text.
        let mut data = header(b"x/y/", b'5', 0, b"").to_vec();
        data.extend_from_slice(&header(b"x/y/l", b'2', 0, b"../.."));
        data.extend_from_slice(&header(b"m", b'2', 0, b"x/y/l/../.."));
        let (_dir, root, result) = run_extract(data);
        result.unwrap();
        assert!(fs::symlink_metadata(root.join("m")).is_err());
    }

    #[test]
    fn colliding_names_fail_instead_of_overwriting() {
        let mut data = file_entry(b"same", b"1");
        data.extend(file_entry(b"same", b"2"));
        let (_dir, _root, result) = run_extract(data);
        assert!(matches!(result, Err(Error::SnapshotCollision(_))));
    }

    #[test]
    fn writes_cannot_pass_through_a_file_parent() {
        let mut data = file_entry(b"a", b"1");
        data.extend(file_entry(b"a/b", b"2"));
        let (_dir, _root, result) = run_extract(data);
        assert!(matches!(result, Err(Error::SnapshotCollision(_))));
    }
}
