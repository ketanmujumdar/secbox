//! Resolving `HEAD` to a commit id, by reading files rather than running git.
//!
//! The broker deliberately refuses anything but a full commit id, so that a
//! ref name can never be resolved to something other than what the person
//! meant. Something still has to do the resolving, and doing it here, by
//! reading a handful of small files as data, avoids starting git at all: no
//! config is loaded, no hook can run, and nothing in the repository can make
//! this process execute anything.
//!
//! This is the user's own checkout, not one written by a sandbox. Even so,
//! every file is size-capped, must be a regular file, and the ref name is
//! checked before it is used to build a path.

use std::path::{Path, PathBuf};

/// Plenty for a HEAD file, a ref file or a worktree pointer.
const MAX_SMALL_FILE: u64 = 4 * 1024;
/// A repository with a very large number of packed refs still fits here.
const MAX_PACKED_REFS: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RefError {
    #[error("this project is not a git repository, so there is no commit to snapshot")]
    NotARepository,
    #[error("this repository has no commits yet")]
    NoCommits,
    #[error("cannot read the repository: {0}")]
    Unreadable(String),
    #[error("the repository's HEAD is not something this can follow")]
    Unsupported,
}

/// True for a full commit id, which is what the broker accepts.
pub fn is_commit_id(text: &str) -> bool {
    (text.len() == 40 || text.len() == 64)
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Finds the directory holding the repository's refs. A checkout made by
/// `git worktree` has a `.git` file pointing at one instead of a directory.
fn git_dir(root: &Path) -> Result<PathBuf, RefError> {
    let dot_git = root.join(".git");
    let meta = std::fs::symlink_metadata(&dot_git).map_err(|_| RefError::NotARepository)?;
    if meta.is_dir() {
        return Ok(dot_git);
    }
    if !meta.is_file() {
        return Err(RefError::NotARepository);
    }
    let text = read_small(&dot_git, MAX_SMALL_FILE)?;
    let target = text
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))
        .ok_or(RefError::NotARepository)?
        .trim();
    if target.is_empty() {
        return Err(RefError::NotARepository);
    }
    let path = Path::new(target);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    if path.is_dir() {
        Ok(path)
    } else {
        Err(RefError::NotARepository)
    }
}

/// Resolves `HEAD` to a full commit id.
pub fn head_commit(root: &Path) -> Result<String, RefError> {
    let dir = git_dir(root)?;
    let head = read_small(&dir.join("HEAD"), MAX_SMALL_FILE)?;
    let head = head.trim();
    if is_commit_id(head) {
        // A detached HEAD holds the id directly.
        return Ok(head.to_owned());
    }
    let Some(name) = head.strip_prefix("ref:").map(str::trim) else {
        return Err(RefError::Unsupported);
    };
    resolve_ref(&dir, name)
}

/// Follows one ref name to a commit id: its own file first, then the packed
/// list. Symbolic refs are followed, with a limit, because a cycle would
/// otherwise never end.
fn resolve_ref(dir: &Path, name: &str) -> Result<String, RefError> {
    let mut name = name.to_owned();
    for _ in 0..8 {
        if !is_safe_ref_name(&name) {
            return Err(RefError::Unsupported);
        }
        let path = dir.join(&name);
        match read_small(&path, MAX_SMALL_FILE) {
            Ok(text) => {
                let text = text.trim().to_owned();
                if is_commit_id(&text) {
                    return Ok(text);
                }
                match text.strip_prefix("ref:").map(str::trim) {
                    Some(next) => name = next.to_owned(),
                    None => return Err(RefError::Unsupported),
                }
            }
            Err(_) => return packed_ref(dir, &name),
        }
    }
    Err(RefError::Unsupported)
}

fn packed_ref(dir: &Path, name: &str) -> Result<String, RefError> {
    let Ok(text) = read_small(&dir.join("packed-refs"), MAX_PACKED_REFS) else {
        return Err(RefError::NoCommits);
    };
    for line in text.lines() {
        if line.starts_with('#') || line.starts_with('^') {
            continue;
        }
        let Some((id, found)) = line.split_once(' ') else {
            continue;
        };
        if found.trim() == name && is_commit_id(id) {
            return Ok(id.to_owned());
        }
    }
    Err(RefError::NoCommits)
}

/// A ref name is used to build a path, so it may not climb out of the
/// repository or name anything odd. These are git's own rules, tightened.
fn is_safe_ref_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name.starts_with("refs/")
        && !name.ends_with('/')
        && name.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.starts_with('.')
                && part.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+' | b'@')
                })
        })
}

fn read_small(path: &Path, limit: u64) -> Result<String, RefError> {
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| RefError::Unreadable(e.kind().to_string()))?;
    if !meta.is_file() {
        return Err(RefError::Unreadable("not a regular file".to_owned()));
    }
    let file =
        std::fs::File::open(path).map_err(|e| RefError::Unreadable(e.kind().to_string()))?;
    let mut text = String::new();
    file.take(limit)
        .read_to_string(&mut text)
        .map_err(|e| RefError::Unreadable(e.kind().to_string()))?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0123456789abcdef0123456789abcdef01234567";

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git/refs/heads")).unwrap();
        dir
    }

    #[test]
    fn a_loose_branch_resolves() {
        let dir = repo();
        std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(dir.path().join(".git/refs/heads/main"), format!("{ID}\n")).unwrap();
        assert_eq!(head_commit(dir.path()).unwrap(), ID);
    }

    #[test]
    fn a_packed_branch_resolves() {
        let dir = repo();
        std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            dir.path().join(".git/packed-refs"),
            format!("# pack-refs with: peeled\n{ID} refs/heads/main\n"),
        )
        .unwrap();
        assert_eq!(head_commit(dir.path()).unwrap(), ID);
    }

    #[test]
    fn a_detached_head_resolves() {
        let dir = repo();
        std::fs::write(dir.path().join(".git/HEAD"), format!("{ID}\n")).unwrap();
        assert_eq!(head_commit(dir.path()).unwrap(), ID);
    }

    #[test]
    fn a_worktree_pointer_is_followed() {
        let outer = tempfile::tempdir().unwrap();
        let real = outer.path().join("real-git");
        std::fs::create_dir_all(real.join("refs/heads")).unwrap();
        std::fs::write(real.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(real.join("refs/heads/main"), format!("{ID}\n")).unwrap();
        let work = outer.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(
            work.join(".git"),
            format!("gitdir: {}\n", real.display()),
        )
        .unwrap();
        assert_eq!(head_commit(&work).unwrap(), ID);
    }

    #[test]
    fn a_project_with_no_repository_says_so() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(head_commit(dir.path()), Err(RefError::NotARepository));
    }

    #[test]
    fn a_branch_with_no_commits_is_not_mistaken_for_one() {
        let dir = repo();
        std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(head_commit(dir.path()), Err(RefError::NoCommits));
    }

    /// A ref name is turned into a path, so this is the one that matters.
    #[test]
    fn a_ref_name_cannot_climb_out_of_the_repository() {
        let dir = repo();
        std::fs::write(
            dir.path().join(".git/HEAD"),
            "ref: refs/heads/../../../../etc/passwd\n",
        )
        .unwrap();
        assert_eq!(head_commit(dir.path()), Err(RefError::Unsupported));
        for name in [
            "refs/heads/../x",
            "notrefs/x",
            "refs/",
            "",
            "refs/heads/a b",
            "refs/heads/.hidden",
        ] {
            assert!(!is_safe_ref_name(name), "{name}");
        }
        assert!(is_safe_ref_name("refs/heads/main"));
        assert!(is_safe_ref_name("refs/remotes/origin/feature-1"));
    }

    #[test]
    fn a_cycle_of_symbolic_refs_ends() {
        let dir = repo();
        std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/a\n").unwrap();
        std::fs::write(dir.path().join(".git/refs/heads/a"), "ref: refs/heads/b\n").unwrap();
        std::fs::write(dir.path().join(".git/refs/heads/b"), "ref: refs/heads/a\n").unwrap();
        assert_eq!(head_commit(dir.path()), Err(RefError::Unsupported));
    }

    #[test]
    fn only_a_full_commit_id_counts_as_one() {
        assert!(is_commit_id(ID));
        assert!(is_commit_id(&"a".repeat(64)));
        assert!(!is_commit_id("HEAD"));
        assert!(!is_commit_id("0123456"));
        assert!(!is_commit_id(&"A".repeat(40)));
        assert!(!is_commit_id(&"g".repeat(40)));
    }
}
