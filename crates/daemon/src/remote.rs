//! Projects that live in a Git remote, reached over SSH.
//!
//! `up git@host:org/repo.git` should not need a clone on the person's
//! machine, and must not put their SSH key in a sandbox. So the daemon fetches
//! on the host, with the person's own `ssh` and SSH agent, into a folder of its
//! own, and every other request then treats that folder as the project:
//!
//! ```text
//! <state>/remote/<host>-<repo>-<hash>/
//!     mirror.git/      bare repository, HEAD detached at the fetched commit
//!     <repo>/          the commit's files, secrets left out, plus a `.git`
//!                      file pointing at mirror.git
//! ```
//!
//! The files are exported with the same snapshot that feeds a sandbox, so a
//! `.env` committed by mistake is left out here too, and nothing from the
//! remote is ever checked out by git. The `.git` file is what lets `up` find
//! the commit the same way it does for a clone on disk: `HEAD` is the fetched
//! commit, and the sandbox's snapshot is taken from the mirror.
//!
//! A fetch never replaces the files under an existing sandbox. Code is copied
//! into a sandbox only when it is created, so a fetch then would leave the
//! folder describing a commit the sandbox does not hold.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use proto::{ApiError, FetchArgs, FetchReport, SandboxState, Source};
use sha2::{Digest, Sha256};

use crate::{audit, bad, gitref, internal, not_found, refused, run, Progress, Service};

/// One fetch at a time. Two at once for the same remote would fight over its
/// folders, and fetches are rare enough that one lock for all is no loss.
static FETCHING: Mutex<()> = Mutex::new(());

const MIRROR: &str = "mirror.git";
const NEXT: &str = ".next";
const OLD: &str = ".old";

/// Answers a `fetch` request.
pub fn fetch(service: &Service, args: &FetchArgs, progress: &mut dyn Progress) -> Result<FetchReport, ApiError> {
    gitbroker::validate_ssh_url(&args.url).map_err(|e| bad(format!("{e}")))?;
    let git_ref = args.git_ref.clone().unwrap_or_else(|| "HEAD".to_owned());
    gitbroker::validate_fetch_ref(&git_ref).map_err(|e| bad(format!("{e}")))?;
    let agent = match &args.ssh_auth_sock {
        Some(path) => Some(usable_agent_socket(Path::new(path)).ok_or_else(|| {
            bad("SSH_AUTH_SOCK does not name a socket that belongs to this user")
        })?),
        None => None,
    };

    let _one_at_a_time = FETCHING.lock().unwrap_or_else(|e| e.into_inner());
    let remote_dir = service.state_dir().join("remote");
    hoststate::ensure_private_dir(&remote_dir).map_err(internal)?;
    let base = remote_dir.join(folder_key(&args.url));
    hoststate::ensure_private_dir(&base).map_err(internal)?;
    let root = base.join(repo_name(&args.url));
    let mirror = base.join(MIRROR);

    let report = |fetched: bool, root: &Path| -> Result<FetchReport, ApiError> {
        let project = service.open(&root.to_string_lossy())?;
        Ok(FetchReport {
            url: args.url.clone(),
            project_path: project.root.to_string_lossy().into_owned(),
            commit: gitref::head_commit(&project.root).ok(),
            git_ref: git_ref.clone(),
            fetched,
        })
    };

    if root.is_dir() {
        let project = service.open(&root.to_string_lossy())?;
        if !args.fetch || run::status_of(service, &project)? != SandboxState::None {
            return report(false, &root);
        }
    } else if !args.fetch {
        return Err(not_found(format!(
            "this remote has not been fetched yet. Run: {} up {}",
            brand::CLI_BIN,
            args.url
        )));
    }

    progress.step("fetch", Some(&format!("{} {}", args.url, git_ref)));
    let commit = service
        .deps()
        .fetch_remote(&mirror, &args.url, &git_ref, agent.as_deref())
        .map_err(|reason| refused(explain(&reason)))?;
    if !gitref::is_commit_id(&commit) {
        return Err(internal("the fetch did not name a commit"));
    }

    // Exported beside the old copy, then swapped in, so a failure part way
    // leaves the last good copy where it was.
    progress.step("export", Some(&commit));
    let next = base.join(NEXT);
    let old = base.join(OLD);
    remove_leftover(&next)?;
    remove_leftover(&old)?;
    service
        .deps()
        .snapshot(&mirror, &Source::Commit { revision: commit.clone() }, &next)
        .map_err(|reason| internal(format!("cannot export the fetched commit: {reason}")))?;
    write_git_pointer(&next, &mirror)?;
    if root.exists() {
        fs::rename(&root, &old).map_err(|e| internal(format!("cannot move the old copy aside: {}", e.kind())))?;
    }
    fs::rename(&next, &root).map_err(|e| internal(format!("cannot put the new copy in place: {}", e.kind())))?;
    remove_leftover(&old)?;

    let fetched = report(true, &root)?;
    service.record(
        audit::entry(service.deps().now_unix(), audit::Action::RemoteFetched)
            .subject(&args.url)
            .detail(format!("{git_ref} at {commit}")),
    );
    Ok(fetched)
}

/// An agent socket the daemon may hand to `ssh`: a socket, owned by the user
/// the daemon runs as. Anything else is ignored rather than trusted.
pub fn usable_agent_socket(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let meta = fs::metadata(path).ok()?;
    let mine = meta.uid() == nix::unistd::getuid().as_raw();
    (meta.file_type().is_socket() && mine).then(|| path.to_path_buf())
}

fn remove_leftover(path: &Path) -> Result<(), ApiError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path)
            .map_err(|e| internal(format!("cannot clear an unfinished fetch: {}", e.kind()))),
        Ok(_) => fs::remove_file(path)
            .map_err(|e| internal(format!("cannot clear an unfinished fetch: {}", e.kind()))),
        Err(_) => Ok(()),
    }
}

/// Writes `.git` as a file naming the mirror, as `git worktree` does. The
/// export never holds a `.git` of its own, because git cannot track one.
fn write_git_pointer(tree: &Path, mirror: &Path) -> Result<(), ApiError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tree.join(".git"))
        .map_err(|e| internal(format!("cannot write the project's .git pointer: {}", e.kind())))?;
    writeln!(file, "gitdir: {}", mirror.display())
        .map_err(|e| internal(format!("cannot write the project's .git pointer: {}", e.kind())))
}

/// The folder for one remote: readable, and unique to the URL as written.
fn folder_key(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    let hash: String = digest[..6].iter().map(|b| format!("{b:02x}")).collect();
    let words = format!("{}-{}", host_of(url), repo_name(url));
    let mut clean = String::new();
    for c in words.chars().take(60) {
        let c = c.to_ascii_lowercase();
        let c = if c.is_ascii_alphanumeric() { c } else { '-' };
        if !(c == '-' && clean.ends_with('-')) {
            clean.push(c);
        }
    }
    let clean = clean.trim_matches('-');
    if clean.is_empty() {
        hash
    } else {
        format!("{clean}-{hash}")
    }
}

fn host_of(url: &str) -> &str {
    let rest = url.strip_prefix("ssh://").unwrap_or(url);
    let authority = rest.split(['/', ':']).next().unwrap_or("");
    authority.rsplit('@').next().unwrap_or(authority)
}

/// The project folder's name: the repository's, so the project's identity
/// reads as it would for a clone.
fn repo_name(url: &str) -> String {
    let last = url.trim_end_matches('/').rsplit(['/', ':']).next().unwrap_or("");
    let last = last.strip_suffix(".git").unwrap_or(last);
    let clean: String = last
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') { c } else { '-' })
        .collect();
    let clean = clean.trim_start_matches('.');
    if clean.is_empty() || clean == MIRROR {
        "repo".to_owned()
    } else {
        clean.to_owned()
    }
}

/// Adds what to do about the failures people actually hit. `ssh`'s own words
/// stay first, already escaped by the broker.
fn explain(reason: &str) -> String {
    let mut text = format!("the fetch failed: {reason}");
    let hint = if reason.contains("Host key verification failed") {
        Some(
            "The host's key is not in your known_hosts. Check it and add it by connecting once yourself, for example: ssh -T git@github.com",
        )
    } else if reason.contains("Permission denied") || reason.contains("publickey") {
        Some(
            "No key was accepted. Load yours into your SSH agent with ssh-add, then try again. A key with a passphrase works only through the agent, because nothing can ask for it.",
        )
    } else if reason.contains("Could not resolve hostname") || reason.contains("Connection timed out") {
        Some("The host could not be reached from this machine.")
    } else if reason.contains("not our ref") || reason.contains("couldn't find remote ref") {
        Some("The remote has no branch, tag or commit by that name.")
    } else {
        None
    };
    if let Some(hint) = hint {
        text.push('\n');
        text.push_str(hint);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_gets_a_readable_folder_unique_to_its_url() {
        let a = folder_key("git@github.com:org/demo_shop.git");
        assert!(a.starts_with("github-com-demo-shop-"), "{a}");
        assert_ne!(a, folder_key("git@github.com:other/demo_shop.git"));
        assert_eq!(a, folder_key("git@github.com:org/demo_shop.git"));
        let b = folder_key("ssh://git@host.example:2222/srv/x.git");
        assert!(b.starts_with("host-example-x-"), "{b}");
    }

    #[test]
    fn the_project_folder_is_named_after_the_repository() {
        assert_eq!(repo_name("git@github.com:org/demo_shop.git"), "demo_shop");
        assert_eq!(repo_name("ssh://host/srv/app/"), "app");
        assert_eq!(repo_name("host:repo"), "repo");
        assert_eq!(repo_name("host:.git"), "repo");
        assert_eq!(repo_name("host:..hidden"), "hidden");
    }

    #[test]
    fn common_failures_say_what_to_do() {
        assert!(explain("Host key verification failed.").contains("known_hosts"));
        assert!(explain("git@github.com: Permission denied (publickey).").contains("ssh-add"));
        assert!(!explain("something else").contains('\n'));
    }

    #[test]
    fn only_a_socket_of_this_user_is_used_as_the_agent() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("agent");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        assert_eq!(usable_agent_socket(&socket), Some(socket.clone()));
        let file = dir.path().join("file");
        fs::write(&file, "").unwrap();
        assert_eq!(usable_agent_socket(&file), None);
        assert_eq!(usable_agent_socket(Path::new("agent")), None);
        assert_eq!(usable_agent_socket(&dir.path().join("missing")), None);
    }
}
