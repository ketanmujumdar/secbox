//! Which way a Git command typed by a person or an agent goes.
//!
//! Any Git command may run in a sandbox's clone ([`proto::GitOp::Run`]). It runs
//! inside the VM, never on the host, so a hook or a setting in the clone only
//! ever runs there. What this decides is whether the command gets the relay to
//! the remote, and which half of it:
//!
//! - `fetch`, `pull` and `ls-remote` get the relay as a fetch (`git-upload-pack`).
//! - `push` gets it as a push (`git-receive-pack`), with every check in
//!   `gitrelay` on the branch list.
//! - Everything else gets no relay. It can't reach the remote with the
//!   person's key, whatever it tries: the clone's `core.sshCommand` only prints
//!   a message, and the sandbox holds no credential.
//!
//! The daemon checks the service Git actually asks for, so these rules are
//! not what keeps a push to `main` out. They keep the arguments plain:
//! the first is a subcommand, so nothing can come before it (`-c`, `-C`,
//! `--git-dir`, `--exec-path`), and a networked command can't name its own
//! remote program (`--upload-pack`, `--receive-pack`, `--exec`).

use gitbroker::GitService;

/// Where a command runs from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// No relay: the command stays inside the sandbox.
    Local,
    /// The relay, for fetching only.
    Relayed(GitService),
}

/// Subcommands that fetch through the relay.
const FETCHES: &[&str] = &["fetch", "pull", "ls-remote"];

/// Options that would let a command choose the program run on the remote.
/// The relay ignores that program anyway; refusing them here gives a clear
/// message instead of a refusal from the relay part way through.
const REMOTE_PROGRAM: &[&str] = &["--upload-pack", "--receive-pack", "--exec"];

/// Options that send data the relay passes on unread, to be acted on by the
/// remote: push options, such as GitLab's `merge_request.create`, and fetch's
/// server options. The relay refuses push options in the branch list too.
const SERVER_OPTIONS: &[&str] = &["-o", "--push-option", "--server-option"];

/// Checks a command's arguments and says which way it goes.
pub fn classify(args: &[String]) -> Result<Route, String> {
    let Some(first) = args.first() else {
        return Err("give a Git command, such as status or log --oneline".to_owned());
    };
    if args.len() > proto::MAX_GIT_ARGS {
        return Err(format!("a Git command may have at most {} arguments", proto::MAX_GIT_ARGS));
    }
    for arg in args {
        if arg.len() > proto::MAX_GIT_ARG_BYTES {
            return Err("one of the arguments is too long".to_owned());
        }
        // Newlines and tabs belong in a commit message; nothing else hidden does.
        if arg.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
            return Err("an argument holds a control character".to_owned());
        }
    }
    if first == "git" {
        return Err("leave out the leading git: give status, not git status".to_owned());
    }
    if !is_subcommand(first) {
        return Err(format!(
            "the first argument must be a Git command such as status or log, not {}. Options before the command, such as -c and -C, are not allowed",
            proto::text::escape_short(first, 60)
        ));
    }
    let route = if FETCHES.contains(&first.as_str()) {
        Route::Relayed(GitService::UploadPack)
    } else if first == "push" {
        Route::Relayed(GitService::ReceivePack)
    } else {
        return Ok(Route::Local);
    };
    // Up to `--`, which ends the options: after it, an argument is a path or a
    // refspec, never an option.
    for arg in args[1..].iter().take_while(|arg| *arg != "--") {
        let option = arg.split_once('=').map_or(arg.as_str(), |(name, _)| name);
        // For ls-remote, and only there, `-u` is short for `--upload-pack`.
        let ls_remote_u = first == "ls-remote" && option.starts_with("-u") && !option.starts_with("--");
        if REMOTE_PROGRAM.contains(&option) || ls_remote_u {
            return Err(format!("{option} chooses the program run on the remote, which a sandbox may not do"));
        }
        if SERVER_OPTIONS.contains(&option) || is_short_cluster_with(option, 'o') {
            return Err(format!("{option} sends options to the remote, which a sandbox may not do"));
        }
    }
    Ok(route)
}

/// The command's name, safe to put in the audit log.
pub fn subcommand(args: &[String]) -> &str {
    args.first().map(String::as_str).filter(|first| is_subcommand(first)).unwrap_or("?")
}

/// A Git subcommand name, or an alias: lowercase letters, digits and dashes,
/// starting with a letter.
fn is_subcommand(text: &str) -> bool {
    text.len() <= 40
        && text.starts_with(|c: char| c.is_ascii_lowercase())
        && text.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `-qo` and the like: short options run together, one of them `letter`.
fn is_short_cluster_with(option: &str, letter: char) -> bool {
    option.len() > 2
        && option.starts_with('-')
        && !option.starts_with("--")
        && option[1..].chars().all(|c| c.is_ascii_alphabetic())
        && option[1..].contains(letter)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn local_commands_stay_local() {
        for line in ["status", "log --oneline -5", "switch -c fix", "stash", "stash pop", "rebase main", "commit -m x", "reset --hard HEAD~1", "remote -v"] {
            assert_eq!(classify(&args(line)), Ok(Route::Local), "{line}");
        }
    }

    #[test]
    fn fetching_and_pushing_go_through_the_relay() {
        for line in ["fetch", "fetch origin main", "pull", "pull --rebase", "pull --no-rebase --autostash", "ls-remote origin"] {
            assert_eq!(classify(&args(line)), Ok(Route::Relayed(GitService::UploadPack)), "{line}");
        }
        for line in ["push", "push -u origin HEAD", "push --set-upstream origin fix"] {
            assert_eq!(classify(&args(line)), Ok(Route::Relayed(GitService::ReceivePack)), "{line}");
        }
    }

    #[test]
    fn nothing_may_come_before_the_command() {
        for line in ["-c core.sshCommand=evil fetch", "-C / status", "--git-dir=/x status", "--exec-path=/tmp push", "-p log", "git status", "Status", ""] {
            assert!(classify(&args(line)).is_err(), "{line}");
        }
        assert!(classify(&[]).is_err());
    }

    #[test]
    fn a_networked_command_cannot_choose_the_remote_program_or_send_options() {
        for line in [
            "fetch --upload-pack=/bin/sh",
            "fetch --upload-pack /bin/sh",
            "pull --upload-pack=x",
            "ls-remote --exec=x",
            "ls-remote -u x",
            "ls-remote -u/bin/sh",
            "push --receive-pack=x",
            "push --exec=x",
            "push -o ci.skip",
            "push -qo ci.skip",
            "push --push-option=merge_request.create",
            "fetch --server-option=x",
        ] {
            assert!(classify(&args(line)).is_err(), "{line}");
        }
        // After `--`, it is a refspec or path, not an option.
        assert!(classify(&args("push origin -- -o")).is_ok());
        // A local command may use these names freely: it has no relay.
        assert_eq!(classify(&args("log -o")), Ok(Route::Local));
    }

    #[test]
    fn hidden_characters_and_huge_commands_are_refused() {
        assert!(classify(&["commit".to_owned(), "-m".to_owned(), "line one\nline two\tok".to_owned()]).is_ok());
        assert!(classify(&["log".to_owned(), "\u{1b}[2J".to_owned()]).is_err());
        assert!(classify(&["log".to_owned(), "a\0b".to_owned()]).is_err());
        assert!(classify(&vec!["status".to_owned(); proto::MAX_GIT_ARGS + 1]).is_err());
        assert!(classify(&["commit".to_owned(), "-m".to_owned(), "x".repeat(proto::MAX_GIT_ARG_BYTES + 1)]).is_err());
    }

    #[test]
    fn the_audit_log_gets_the_command_name_only() {
        assert_eq!(subcommand(&args("commit -m secret-ish")), "commit");
        assert_eq!(subcommand(&args("-c x status")), "?");
    }
}
