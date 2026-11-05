//! The broker-owned bare repository that holds imported bundles.

use std::fs::{self, DirBuilder, File};
use std::io::{self, Read};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::bundle::read_header;
use crate::display::{escape_for_display, escape_stderr};
use crate::error::{Error, Result};
use crate::git::{GitCmd, Protocol};
use crate::review::{self, Review, ReviewLimits};
use crate::validate::validate_commit_id;

const QUARANTINE_PREFIX: &str = "refs/quarantine/";
const BASE_PREFIX: &str = "refs/base/";
const IMPORT_TIMEOUT: Duration = Duration::from_secs(300);

/// A bare repository owned by the broker. Create one per task: [`Quarantine::push`]
/// treats the latest import as the agent's current proposal.
#[derive(Debug)]
pub struct Quarantine {
    dir: PathBuf,
    limits: ReviewLimits,
}

/// One ref taken from a bundle, as it now exists in quarantine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ImportedRef {
    /// The ref name inside the bundle. It passed the allowlist, so it is safe.
    pub bundle_ref: String,
    /// Where it was stored, under `refs/quarantine/<import id>/`.
    pub quarantine_ref: String,
    pub object_id: String,
}

/// What an import stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ImportedBundle {
    pub import_id: u64,
    pub refs: Vec<ImportedRef>,
    pub prerequisites: Vec<String>,
}

impl Quarantine {
    /// Create a new bare repository at `dir`, which must not exist yet. It has no
    /// template, so it starts with no hooks directory at all.
    pub fn create(dir: &Path) -> Result<Self> {
        if !dir.is_absolute() {
            return Err(Error::NotAQuarantine(escape_for_display(
                dir.as_os_str().as_encoded_bytes(),
            )));
        }
        DirBuilder::new().mode(0o700).create(dir).map_err(|err| {
            if err.kind() == io::ErrorKind::AlreadyExists {
                Error::PathExists(escape_for_display(dir.as_os_str().as_encoded_bytes()))
            } else {
                Error::Io(err)
            }
        })?;
        GitCmd::new("init")
            .args([
                "init",
                "--quiet",
                "--bare",
                "--template=",
                "--object-format=sha1",
                "--initial-branch=main",
                "--",
            ])
            .arg(dir)
            .ok()?;
        Self::open(dir)
    }

    /// Open an existing quarantine. The directory is trusted to be the broker's
    /// own. Its config is not audited, though hardened command-line config
    /// overrides what it could say about hooks and helpers.
    pub fn open(dir: &Path) -> Result<Self> {
        let not_one =
            || Error::NotAQuarantine(escape_for_display(dir.as_os_str().as_encoded_bytes()));
        let meta = fs::symlink_metadata(dir).map_err(|_| not_one())?;
        if !meta.is_dir() {
            return Err(not_one());
        }
        let dir = fs::canonicalize(dir)?;
        let out = GitCmd::new("rev-parse")
            .git_dir(&dir)
            .args(["rev-parse", "--is-bare-repository"])
            .run()?;
        if !out.success() || out.stdout_trimmed() != "true" {
            return Err(not_one());
        }
        Ok(Self {
            dir,
            limits: ReviewLimits::default(),
        })
    }

    /// Replace the limits used by [`Quarantine::review`].
    pub fn set_review_limits(&mut self, limits: ReviewLimits) {
        self.limits = limits;
    }

    /// The repository directory.
    pub fn path(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn git(&self, op: &'static str) -> GitCmd {
        GitCmd::new(op).git_dir(&self.dir)
    }

    /// Take in a hostile bundle.
    ///
    /// The file is copied first, so git never reads a path the VM could swap.
    /// Then its header is parsed here and any bad ref rejects the whole bundle,
    /// `git bundle verify` checks prerequisites, and `git fetch` runs with fsck on
    /// and one explicit refspec per ref, all under `refs/quarantine/<import id>/`.
    pub fn import_bundle(&mut self, path: &Path, max_bytes: u64) -> Result<ImportedBundle> {
        let work = tempfile::Builder::new().prefix("gb-import-").tempdir()?;
        let copy = work.path().join("incoming.bundle");
        copy_limited(path, &copy, max_bytes)?;

        let header = read_header(&copy)?;

        let verify = self
            .git("bundle-verify")
            .args(["bundle", "verify", "--quiet"])
            .arg(&copy)
            .timeout(IMPORT_TIMEOUT)
            .run()?;
        if !verify.success() {
            return Err(Error::ImportRejected {
                stderr: escape_stderr(&verify.stderr),
            });
        }

        let import_id = self.next_import_id()?;
        let mut refs = Vec::with_capacity(header.refs.len());
        let mut refspecs = Vec::with_capacity(header.refs.len());
        for (object_id, bundle_ref) in &header.refs {
            let stored = bundle_ref.strip_prefix("refs/").unwrap_or(bundle_ref);
            let quarantine_ref = format!("{QUARANTINE_PREFIX}{import_id}/{stored}");
            refspecs.push(format!("{bundle_ref}:{quarantine_ref}"));
            refs.push(ImportedRef {
                bundle_ref: bundle_ref.clone(),
                quarantine_ref,
                object_id: object_id.clone(),
            });
        }

        let fetched = self
            .git("fetch")
            .protocol(Protocol::File)
            .args([
                "fetch",
                "--quiet",
                "--no-tags",
                "--atomic",
                "--no-write-fetch-head",
                "--no-auto-maintenance",
                "--no-recurse-submodules",
                "--",
            ])
            .arg(&copy)
            .args(&refspecs)
            .timeout(IMPORT_TIMEOUT)
            .run();
        let fetched = match fetched {
            Ok(out) => out,
            Err(err) => {
                self.remove_partial_packs();
                return Err(err);
            }
        };
        if !fetched.success() {
            // index-pack leaves its temporary pack behind when fsck kills it.
            self.remove_partial_packs();
            return Err(Error::ImportRejected {
                stderr: escape_stderr(&fetched.stderr),
            });
        }

        Ok(ImportedBundle {
            import_id,
            refs,
            prerequisites: header.prerequisites,
        })
    }

    /// Fetch a base commit from a trusted repository into `refs/base/`, so that a
    /// bundle with prerequisites can be imported and reviewed against it. Base
    /// commits can be reviewed against, but they cannot be pushed.
    pub fn import_base(&mut self, trusted_repo: &Path, commit: &str) -> Result<()> {
        validate_commit_id(commit)?;
        let source = crate::snapshot::git_dir_of(trusted_repo)?;
        self.git("fetch")
            .protocol(Protocol::File)
            .args([
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-write-fetch-head",
                "--no-auto-maintenance",
                "--no-recurse-submodules",
                "--",
            ])
            .arg(&source)
            .arg(format!("{commit}:{BASE_PREFIX}{commit}"))
            .timeout(IMPORT_TIMEOUT)
            .ok()?;
        Ok(())
    }

    /// Review `base..tip`. With no base, the tip is compared to the empty tree.
    pub fn review(&self, base: Option<&str>, tip: &str) -> Result<Review> {
        validate_commit_id(tip)?;
        self.require_reachable(tip, &[QUARANTINE_PREFIX, BASE_PREFIX])?;
        if let Some(base) = base {
            validate_commit_id(base)?;
            self.require_reachable(base, &[QUARANTINE_PREFIX, BASE_PREFIX])?;
        }
        review::run(self, base, tip, &self.limits)
    }

    pub(crate) fn tree_of(&self, commit: &str) -> Result<String> {
        let out = self
            .git("rev-parse")
            .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
            .arg(format!("{commit}^{{tree}}"))
            .run()?;
        if !out.success() {
            return Err(Error::CommitNotInQuarantine(commit.to_string()));
        }
        Ok(out.stdout_trimmed())
    }

    /// The object must be a commit reachable from a ref under one of `prefixes`.
    /// An object merely being present is not enough.
    pub(crate) fn require_reachable(&self, commit: &str, prefixes: &[&str]) -> Result<()> {
        let missing = || Error::CommitNotInQuarantine(commit.to_string());
        let kind = self
            .git("cat-file")
            .args(["cat-file", "-t", commit])
            .run()?;
        if !kind.success() || kind.stdout_trimmed() != "commit" {
            return Err(missing());
        }
        let out = self
            .git("for-each-ref")
            .args([
                "for-each-ref",
                "--count=1",
                "--format=%(refname)",
                "--contains",
                commit,
            ])
            .args(prefixes)
            .ok()?;
        if out.stdout_trimmed().is_empty() {
            return Err(missing());
        }
        Ok(())
    }

    /// Object ids at the refs of the most recent import, peeling annotated tags.
    pub(crate) fn latest_import_tips(&self) -> Result<Vec<String>> {
        let mut latest: Option<u64> = None;
        let mut tips: Vec<(u64, String)> = Vec::new();
        for (name, oids) in self.list_quarantine_refs()? {
            let Some(id) = import_id_of(&name) else {
                continue;
            };
            latest = Some(latest.map_or(id, |seen| seen.max(id)));
            tips.extend(oids.into_iter().map(|oid| (id, oid)));
        }
        Ok(tips
            .into_iter()
            .filter(|(id, _)| Some(*id) == latest)
            .map(|(_, oid)| oid)
            .collect())
    }

    fn list_quarantine_refs(&self) -> Result<Vec<(String, Vec<String>)>> {
        let out = self
            .git("for-each-ref")
            .args([
                "for-each-ref",
                "--format=%(refname) %(objectname) %(*objectname)",
                QUARANTINE_PREFIX,
            ])
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        Ok(text
            .lines()
            .filter_map(|line| {
                let mut fields = line.split(' ');
                let name = fields.next()?.to_string();
                let oids = fields
                    .filter(|f| !f.is_empty())
                    .map(str::to_string)
                    .collect();
                Some((name, oids))
            })
            .collect())
    }

    fn next_import_id(&self) -> Result<u64> {
        let highest = self
            .list_quarantine_refs()?
            .iter()
            .filter_map(|(name, _)| import_id_of(name))
            .max()
            .unwrap_or(0);
        Ok(highest + 1)
    }

    /// Best effort: a leftover temporary pack is junk, and it is not referenced.
    fn remove_partial_packs(&self) {
        let Ok(entries) = fs::read_dir(self.dir.join("objects/pack")) else {
            return;
        };
        for entry in entries.flatten() {
            if entry
                .file_name()
                .as_encoded_bytes()
                .starts_with(b"tmp_pack_")
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

fn import_id_of(refname: &str) -> Option<u64> {
    refname
        .strip_prefix(QUARANTINE_PREFIX)?
        .split('/')
        .next()?
        .parse()
        .ok()
}

/// Copy the bundle into broker-owned space, refusing symlinks, FIFOs, devices and
/// anything over the limit. The limit is enforced on the bytes actually copied,
/// so a file that grows after the size check cannot get past it.
fn copy_limited(source: &Path, dest: &Path, max_bytes: u64) -> Result<()> {
    let before = fs::symlink_metadata(source)?;
    if !before.is_file() {
        return Err(Error::BundleNotRegularFile);
    }
    if before.len() > max_bytes {
        return Err(Error::BundleTooLarge {
            size: before.len(),
            limit: max_bytes,
        });
    }
    let mut input = File::open(source)?;
    let opened = input.metadata()?;
    if !opened.is_file() || opened.ino() != before.ino() || opened.dev() != before.dev() {
        return Err(Error::BundleNotRegularFile);
    }
    let mut output = File::create(dest)?;
    let copied = io::copy(&mut (&mut input).take(max_bytes + 1), &mut output)?;
    if copied > max_bytes {
        return Err(Error::BundleTooLarge {
            size: copied,
            limit: max_bytes,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_ids_come_from_ref_names() {
        assert_eq!(import_id_of("refs/quarantine/12/heads/main"), Some(12));
        assert_eq!(import_id_of("refs/quarantine/x/heads/main"), None);
        assert_eq!(import_id_of("refs/heads/main"), None);
    }

    #[test]
    fn create_refuses_an_existing_or_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            Quarantine::create(dir.path()),
            Err(Error::PathExists(_))
        ));
        assert!(matches!(
            Quarantine::create(Path::new("relative")),
            Err(Error::NotAQuarantine(_))
        ));
    }

    #[test]
    fn open_refuses_an_ordinary_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            Quarantine::open(dir.path()),
            Err(Error::NotAQuarantine(_))
        ));
    }

    #[test]
    fn new_quarantine_has_no_hooks_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.git");
        let quarantine = Quarantine::create(&path).unwrap();
        assert!(!quarantine.path().join("hooks").exists());
    }
}
