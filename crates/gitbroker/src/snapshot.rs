//! Export a commit from a trusted repository into a directory for the VM.
//!
//! The archive is read by this crate, and excluded files are never written to the
//! destination. A final walk then checks the result on disk, so the guarantee
//! does not rest on the reader alone. Symlinks are created last, and each must
//! resolve inside the destination, so nothing can be written through one.

use std::ffi::OsStr;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, ErrorKind, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, DirBuilderExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::display::{escape_for_display, fold_component};
use crate::error::{Error, Result};
use crate::git::GitCmd;
use crate::tar::{self, Entry, EntryKind};
use crate::validate::validate_commit_id;

const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_DEPTH: usize = 256;
const KEPT_ENV_EXAMPLE: &[u8] = b".env.example";

/// Why an entry is not in the snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    /// `.env` or `.env.*`, in any case, or anything inside such a directory.
    EnvFile,
    /// A path component that would act as git metadata.
    GitMetadata,
    /// A symlink that leaves the snapshot or does not resolve inside it.
    EscapingSymlink,
}

/// One entry left out of the snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExcludedEntry {
    /// Escaped and safe to print.
    pub path: String,
    pub reason: ExclusionReason,
}

/// What the snapshot contains and what it left out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SnapshotReport {
    pub commit: String,
    pub files_written: u64,
    pub symlinks_written: u64,
    pub excluded: Vec<ExcludedEntry>,
}

/// Resolve the git directory of a trusted repository without discovery, so a
/// path inside some other repository cannot silently select that repository.
pub(crate) fn git_dir_of(repo: &Path) -> Result<PathBuf> {
    let not_repo = || Error::NotAQuarantine(escape_for_display(repo.as_os_str().as_bytes()));
    if !repo.is_absolute() {
        return Err(not_repo());
    }
    let dot_git = repo.join(".git");
    if dot_git.exists() {
        Ok(dot_git)
    } else if repo.join("HEAD").is_file() && repo.join("objects").is_dir() {
        Ok(repo.to_path_buf())
    } else {
        Err(not_repo())
    }
}

/// True for `.env` and `.env.*` after case and invisible-character folding.
fn is_env_name(component: &[u8]) -> bool {
    let folded = fold_component(component);
    folded == ".env" || folded.starts_with(".env.")
}

fn is_git_metadata_name(component: &[u8]) -> bool {
    let folded = fold_component(component);
    folded == ".git" || folded == "git~1"
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

struct Extraction<'a> {
    root: &'a Path,
    report: SnapshotReport,
    deferred_links: Vec<(Vec<u8>, Vec<u8>)>,
}

impl Extraction<'_> {
    fn exclude(&mut self, path: &[u8], reason: ExclusionReason) {
        self.report.excluded.push(ExcludedEntry {
            path: escape_for_display(path),
            reason,
        });
    }

    /// The reason this entry is left out, if it is.
    fn exclusion(entry: &Entry, parts: &[&[u8]]) -> Option<ExclusionReason> {
        let is_file = matches!(entry.kind, EntryKind::File { .. });
        for (index, part) in parts.iter().enumerate() {
            if is_git_metadata_name(part) {
                return Some(ExclusionReason::GitMetadata);
            }
            if is_env_name(part) {
                // The exception is only for that exact name on a regular file. A
                // directory or symlink called `.env.example` is not an example.
                let last = index + 1 == parts.len();
                if !(last && is_file && *part == KEPT_ENV_EXAMPLE) {
                    return Some(ExclusionReason::EnvFile);
                }
            }
        }
        None
    }

    fn accept(&mut self, entry: Entry, body: &mut dyn Read) -> Result<()> {
        let parts = checked_components(&entry.path)?;
        if let Some(reason) = Self::exclusion(&entry, &parts) {
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
                let copied = io::copy(body, &mut file)?;
                if copied != entry.size {
                    return Err(Error::SnapshotFormat("archive is truncated"));
                }
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
    /// remove anything that should not be there.
    fn sweep(&mut self) -> Result<()> {
        let mut pending = vec![self.root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for item in fs::read_dir(&dir)? {
                let item = item?;
                let path = item.path();
                let name = item.file_name();
                let raw = name.as_bytes();
                let meta = fs::symlink_metadata(&path)?;
                let kept_example = raw == KEPT_ENV_EXAMPLE && meta.is_file();
                let reason = if is_git_metadata_name(raw) {
                    Some(ExclusionReason::GitMetadata)
                } else if is_env_name(raw) && !kept_example {
                    Some(ExclusionReason::EnvFile)
                } else {
                    None
                };
                if let Some(reason) = reason {
                    if meta.is_dir() {
                        fs::remove_dir_all(&path)?;
                    } else {
                        fs::remove_file(&path)?;
                    }
                    let shown = path.strip_prefix(self.root).unwrap_or(&path);
                    self.exclude(shown.as_os_str().as_bytes(), reason);
                } else if meta.is_dir() {
                    pending.push(path);
                }
            }
        }
        Ok(())
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
        },
        deferred_links: Vec::new(),
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
/// `.env` and `.env.*` are left out in any letter case and in any directory, and
/// are listed in the report. The one exception is a regular file named exactly
/// `.env.example`. Any failure removes `dest`, so a partial snapshot never stays.
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
    fn env_names() {
        for name in [
            ".env",
            ".env.local",
            ".ENV",
            ".Env.Production",
            ".env.example",
            ".e\u{200c}nv",
        ] {
            assert!(is_env_name(name.as_bytes()), "{name}");
        }
        for name in [".envrc", "env", ".environment", "x.env", ".env-local"] {
            assert!(!is_env_name(name.as_bytes()), "{name}");
        }
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
