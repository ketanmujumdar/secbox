//! Fetching a project from an SSH remote into a mirror the host owns.
//!
//! The user's SSH key never leaves the host. Git runs here, through the
//! hardened [`GitCmd`], and the only SSH it may use is one fixed command line:
//! the system `ssh` with options that refuse to prompt, refuse an unknown host
//! key, and forward nothing. The key is reached through the user's SSH agent
//! when a socket is given, or through `ssh`'s own default key files otherwise.
//! A sandbox only ever receives files exported from the mirror afterwards.
//!
//! ```text
//! fetch_ssh(mirror, url, ref) -> commit      git fetch over ssh, with fsck
//! snapshot(mirror, commit, dest)             files only, secrets left out
//! ```
//!
//! # What reaches `ssh`
//!
//! Git starts `core.sshCommand` through `sh`, so the program's path is limited
//! to shell-safe characters, as a credential helper's is. Git appends the host
//! and the remote command itself; the URL is checked first so that neither can
//! be read as an option. Options given with `-o` beat the user's
//! `~/.ssh/config`, so these hold whatever it says. Settings the command line
//! leaves alone, such as `ProxyJump` or `IdentityFile`, still come from that
//! file. It is the user's own, which is how a fetch through a bastion or with a
//! particular key keeps working.
//!
//! `ssh` finds `~/.ssh` from the password database, not from `HOME`, so the
//! empty temporary `HOME` git runs with does not hide the user's
//! `known_hosts`, config or keys from it.

use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git::{GitCmd, Protocol, NETWORK_TIMEOUT};
use crate::validate::validate_commit_id;

/// Where `ssh` is looked for, in order. Nothing searches `PATH`.
const SSH_CANDIDATES: &[&str] = &["/usr/bin/ssh", "/usr/local/bin/ssh", "/opt/homebrew/bin/ssh"];

/// Every fetch's `ssh` options. A command-line option wins over the same
/// option in any config file.
const SSH_OPTIONS: &[&str] = &[
    // Never ask anything: a passphrase, a password or a yes/no. There is no
    // one to answer, and a prompt would hang until the time limit.
    "BatchMode=yes",
    // Refuse a host whose key is not already in known_hosts, and never write
    // a new key there. Trusting a host is for a person, with `ssh` itself.
    "StrictHostKeyChecking=yes",
    "UpdateHostKeys=no",
    // Nothing goes the other way: no agent, no X11, no ports.
    "ForwardAgent=no",
    "ForwardX11=no",
    "ClearAllForwardings=yes",
    "PermitLocalCommand=no",
    "RequestTTY=no",
    // A shared connection could have been opened with other options.
    "ControlMaster=no",
    "ControlPath=none",
    "ConnectTimeout=30",
    "ServerAliveInterval=15",
    "ServerAliveCountMax=4",
];

const MAX_URL_LEN: usize = 2048;
const MAX_REF_LEN: usize = 200;

/// Where the fetched commit is kept in the mirror. Fixed, so a remote cannot
/// choose the name of anything written here.
const FETCHED_REF: &str = "refs/fetched/head";

/// The `ssh` program and, if there is one, the agent socket it may use.
#[derive(Debug, Clone)]
pub struct SshTransport {
    program: PathBuf,
    agent_socket: Option<PathBuf>,
}

impl SshTransport {
    /// The first `ssh` in the fixed list that exists.
    pub fn find_program() -> Option<PathBuf> {
        SSH_CANDIDATES
            .iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
    }

    /// Checks the program as a credential helper is checked: absolute,
    /// shell-safe, a regular executable file nobody else may write. The socket,
    /// if given, must be an absolute path to a socket. Whether it belongs to
    /// the right user is for the caller, which knows who that is.
    pub fn new(program: &Path, agent_socket: Option<&Path>) -> Result<Self> {
        let bad = Error::InvalidSsh;
        let text = program.to_str().ok_or(bad("the ssh path is not UTF-8"))?;
        if !program.is_absolute() {
            return Err(bad("the ssh path is not absolute"));
        }
        if !text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'))
        {
            return Err(bad("the ssh path holds characters unsafe for a shell"));
        }
        // Followed, because /usr/bin/ssh is a link on some systems. What it
        // points at is what runs, so that is what is checked.
        let meta = fs::metadata(program).map_err(|_| bad("ssh cannot be found"))?;
        if !meta.is_file() {
            return Err(bad("ssh is not a regular file"));
        }
        let mode = meta.permissions().mode();
        if mode & 0o111 == 0 {
            return Err(bad("ssh is not executable"));
        }
        if mode & 0o022 != 0 {
            return Err(bad("ssh is writable by group or others"));
        }
        if let Some(socket) = agent_socket {
            if !socket.is_absolute() || socket.to_str().is_none_or(|s| s.chars().any(char::is_control)) {
                return Err(bad("the agent socket path is not a plain absolute path"));
            }
            let meta = fs::metadata(socket).map_err(|_| bad("the agent socket cannot be found"))?;
            if !meta.file_type().is_socket() {
                return Err(bad("the agent socket path is not a socket"));
            }
        }
        Ok(Self {
            program: program.to_path_buf(),
            agent_socket: agent_socket.map(Path::to_path_buf),
        })
    }

    /// The value of `core.sshCommand`.
    pub(crate) fn command_line(&self) -> String {
        let mut line = self.program.to_string_lossy().into_owned();
        for option in SSH_OPTIONS {
            line.push_str(" -o ");
            line.push_str(option);
        }
        line
    }

    pub(crate) fn agent_socket(&self) -> Option<&Path> {
        self.agent_socket.as_deref()
    }
}

/// Checks an SSH remote: `ssh://[user@]host[:port]/path` or `[user@]host:path`.
///
/// Strict on purpose. Nothing may start with `-`, so nothing can become an
/// option to `ssh` or to git; no `::`, so no transport helper; nothing outside
/// printable ASCII and no percent sign, so nothing is hidden or decoded later;
/// and no `..` in the path.
pub fn validate_ssh_url(url: &str) -> Result<()> {
    let bad = Error::InvalidRemote;
    if url.is_empty() || url.len() > MAX_URL_LEN {
        return Err(bad("empty or too long"));
    }
    if url.bytes().any(|b| !(0x21..0x7f).contains(&b)) {
        return Err(bad("contains whitespace, control or non-ASCII characters"));
    }
    if url.starts_with('-') {
        return Err(bad("begins with a dash"));
    }
    if url.contains("::") || url.contains('%') || url.contains('\\') {
        return Err(bad("contains '::', '%' or a backslash"));
    }
    let (authority, path) = if let Some(rest) = url.strip_prefix("ssh://") {
        let (authority, path) = rest.split_once('/').ok_or(bad("has no path"))?;
        let (host_part, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };
        if let Some(port) = port {
            let ok = !port.is_empty()
                && port.len() <= 5
                && port.bytes().all(|b| b.is_ascii_digit())
                && port.parse::<u32>().is_ok_and(|p| (1..=65535).contains(&p));
            if !ok {
                return Err(bad("has an invalid port"));
            }
        }
        (host_part, path)
    } else if url.contains("://") {
        return Err(bad("only ssh:// and user@host:path remotes are accepted here"));
    } else {
        let (authority, path) = url.split_once(':').ok_or(bad("is not an SSH remote"))?;
        if authority.contains('/') {
            return Err(bad("is not an SSH remote"));
        }
        (authority, path)
    };
    let host = match authority.split_once('@') {
        Some((user, host)) => {
            let user_ok = !user.is_empty()
                && user.len() <= 64
                && !user.starts_with('-')
                && user
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
            if !user_ok {
                return Err(bad("has an invalid user name"));
            }
            host
        }
        None => authority,
    };
    let host_ok = !host.is_empty()
        && host.len() <= 253
        && !host.starts_with(['-', '.'])
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'));
    if !host_ok {
        return Err(bad("has an invalid host name"));
    }
    let path_ok = !path.is_empty()
        && !path.starts_with('-')
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/' | b'~' | b'+'))
        && !path.split('/').any(|part| part == "..")
        && !path.trim_matches('/').is_empty();
    if !path_ok {
        return Err(bad("has an invalid repository path"));
    }
    Ok(())
}

/// Checks the branch, tag or commit to fetch. `HEAD` means the remote's
/// default branch.
pub fn validate_fetch_ref(name: &str) -> Result<()> {
    let bad = || Error::BadRefName(crate::display::escape_for_display(name.as_bytes()));
    if name == "HEAD" || validate_commit_id(name).is_ok() {
        return Ok(());
    }
    let ok = !name.is_empty()
        && name.len() <= MAX_REF_LEN
        && !name.starts_with(['-', '/', '.'])
        && !name.ends_with(['/', '.'])
        && !name.ends_with(".lock")
        && !name.contains("..")
        && !name.contains("//")
        && !name.contains("/.")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'));
    if ok {
        Ok(())
    } else {
        Err(bad())
    }
}

/// Fetches `git_ref` from `url` into the bare repository at `mirror`, making
/// it if needed, and points the mirror's `HEAD` at the commit fetched. Returns
/// that commit's full id.
///
/// Objects are checked with fsck on the way in, as for a bundle, and no
/// submodule, tag or hook comes along.
pub fn fetch_ssh(mirror: &Path, url: &str, git_ref: &str, ssh: &SshTransport) -> Result<String> {
    validate_ssh_url(url)?;
    validate_fetch_ref(git_ref)?;
    if !mirror.is_absolute() {
        return Err(Error::NotAQuarantine(crate::display::escape_for_display(
            mirror.as_os_str().as_encoded_bytes(),
        )));
    }
    if !mirror.join("HEAD").is_file() {
        GitCmd::new("init")
            .args(["init", "--bare", "--quiet"])
            .arg(mirror)
            .ok()?;
    }
    let refspec = format!("+{git_ref}:{FETCHED_REF}");
    GitCmd::new("fetch")
        .git_dir(mirror)
        .protocol(Protocol::Ssh)
        .ssh(ssh)
        .timeout(NETWORK_TIMEOUT)
        .args([
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            "--quiet",
            "--end-of-options",
            url,
            &refspec,
        ])
        .ok()?;
    let commit = GitCmd::new("rev-parse")
        .git_dir(mirror)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &format!("{FETCHED_REF}^{{commit}}"),
        ])
        .ok()?
        .stdout_trimmed();
    validate_commit_id(&commit)?;
    GitCmd::new("update-ref")
        .git_dir(mirror)
        .args(["update-ref", "--no-deref", "HEAD", &commit])
        .ok()?;
    Ok(commit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_remotes_in_both_forms_are_accepted() {
        for good in [
            "git@github.com:org/repo.git",
            "git@github.com:org/repo",
            "github.com:org/repo.git",
            "git@gitlab.example.com:group/sub/repo.git",
            "ssh://git@github.com/org/repo.git",
            "ssh://git@host.example:2222/srv/git/repo.git",
            "ssh://host/~user/repo.git",
            "user.name_1@10.0.0.5:/srv/repos/a+b.git",
        ] {
            assert!(validate_ssh_url(good).is_ok(), "{good}");
        }
    }

    #[test]
    fn anything_that_could_become_an_option_or_another_transport_is_refused() {
        for bad in [
            "",
            "-oProxyCommand=touch /tmp/x:repo",
            "ssh://-oProxyCommand=x/r",
            "ssh://git@-oProxyCommand=x/r",
            "ssh://git@host/-repo",
            "git@host:-repo",
            "-git@host:repo",
            "ext::sh -c touch% /tmp/x",
            "fd::3",
            "https://github.com/org/repo.git",
            "file:///tmp/repo",
            "git://host/repo",
            "/local/path",
            "relative/path",
            "git@host:org/../../etc",
            "git@host:org/repo%0a",
            "git@host:org/re po",
            "git@host:org/repo\n",
            "git@ho st:repo",
            "git@host:",
            "git@host:/",
            "ssh://git@host",
            "ssh://git@host:99999/r",
            "ssh://git@host:/r",
            "ssh://git@[::1]/r",
            "git@.host:r",
            "a;b@host:r",
            "git@host:org/r\u{e9}po",
            "git@host:org\\repo",
        ] {
            assert!(validate_ssh_url(bad).is_err(), "{bad:?}");
        }
        assert!(validate_ssh_url(&format!("git@host:{}", "a".repeat(MAX_URL_LEN))).is_err());
    }

    #[test]
    fn fetch_refs_are_plain_names() {
        for good in ["HEAD", "main", "feature/a-b_c.d", "v1.2.3", &"a".repeat(40)] {
            assert!(validate_fetch_ref(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "-main",
            "--upload-pack=x",
            "/main",
            ".hidden",
            "a/.hidden",
            "main/",
            "main.",
            "a.lock",
            "a..b",
            "a//b",
            "a b",
            "a:b",
            "+main",
            "main@{1}",
            "a~1",
            "a^",
            "*",
            "refs/heads/m\nain",
        ] {
            assert!(validate_fetch_ref(bad).is_err(), "{bad:?}");
        }
    }

    fn fake_ssh(dir: &Path, mode: u32) -> PathBuf {
        let path = dir.join("ssh");
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn the_ssh_command_line_is_the_program_and_fixed_options_only() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = SshTransport::new(&fake_ssh(dir.path(), 0o755), None).unwrap();
        let line = ssh.command_line();
        assert!(line.starts_with(&format!("{} -o ", dir.path().join("ssh").display())));
        for option in ["BatchMode=yes", "StrictHostKeyChecking=yes", "ForwardAgent=no", "ClearAllForwardings=yes"] {
            assert!(line.contains(&format!("-o {option}")), "{line}");
        }
        assert!(!line.contains(';') && !line.contains('$') && !line.contains('\''));
    }

    #[test]
    fn an_unsafe_ssh_program_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(SshTransport::new(Path::new("ssh"), None).is_err());
        assert!(SshTransport::new(&fake_ssh(dir.path(), 0o644), None).is_err(), "not executable");
        assert!(SshTransport::new(&fake_ssh(dir.path(), 0o777), None).is_err(), "writable by others");
        let odd = dir.path().join("my ssh");
        fs::write(&odd, "").unwrap();
        fs::set_permissions(&odd, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(SshTransport::new(&odd, None).is_err(), "a space would split the shell word");
        assert!(SshTransport::new(dir.path(), None).is_err(), "a folder");
    }

    /// Runs system git for setting up a test repository only. The code under
    /// test never runs git this way.
    fn plain_git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// A stand-in for `ssh` that records how it was called and then runs the
    /// remote command locally, as a real server would at the other end.
    fn loopback_ssh(dir: &Path) -> PathBuf {
        let path = dir.join("ssh");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {log}/args\nprintf '%s' \"$SSH_AUTH_SOCK\" > {log}/sock\nfor last; do :; done\nexec /bin/sh -c \"$last\"\n",
            log = dir.display()
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Needs git 2.50 or newer, like every other test here that runs git.
    #[test]
    fn a_fetch_over_ssh_lands_one_commit_in_the_mirror_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        plain_git(&source, &["init", "-q", "-b", "main"]);
        fs::write(source.join("README"), "hello\n").unwrap();
        plain_git(&source, &["add", "README"]);
        plain_git(&source, &["commit", "-q", "-m", "one"]);
        plain_git(&source, &["tag", "v1"]);
        fs::write(source.join("README"), "two\n").unwrap();
        plain_git(&source, &["commit", "-q", "-am", "two"]);

        let socket = dir.path().join("agent");
        let _agent = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let ssh = SshTransport::new(&loopback_ssh(dir.path()), Some(&socket)).unwrap();
        let mirror = dir.path().join("mirror.git");
        let url = format!("git@example.invalid:{}", source.display());

        let commit = fetch_ssh(&mirror, &url, "main", &ssh).unwrap();
        assert!(validate_commit_id(&commit).is_ok());
        assert_eq!(fs::read_to_string(mirror.join("HEAD")).unwrap().trim(), commit);

        let args = fs::read_to_string(dir.path().join("args")).unwrap();
        for option in ["BatchMode=yes", "StrictHostKeyChecking=yes", "ForwardAgent=no"] {
            assert!(args.contains(option), "{args}");
        }
        assert!(args.contains("git@example.invalid"), "{args}");
        assert_eq!(fs::read_to_string(dir.path().join("sock")).unwrap(), socket.to_str().unwrap());
        // No tag came along, and the fetched commit has the newest content.
        assert!(!mirror.join("refs/tags/v1").exists());
        let dest = dir.path().join("tree");
        crate::snapshot(&mirror, &commit, &dest).unwrap();
        assert_eq!(fs::read_to_string(dest.join("README")).unwrap(), "two\n");

        // An older tag can be fetched into the same mirror.
        let tagged = fetch_ssh(&mirror, &url, "v1", &ssh).unwrap();
        assert_ne!(tagged, commit);
    }

    #[test]
    fn the_agent_socket_must_be_a_socket() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = fake_ssh(dir.path(), 0o755);
        assert!(SshTransport::new(&ssh, Some(&ssh)).is_err(), "a file is not a socket");
        assert!(SshTransport::new(&ssh, Some(Path::new("agent.sock"))).is_err());
        let socket = dir.path().join("agent.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let transport = SshTransport::new(&ssh, Some(&socket)).unwrap();
        assert_eq!(transport.agent_socket(), Some(socket.as_path()));
    }
}
