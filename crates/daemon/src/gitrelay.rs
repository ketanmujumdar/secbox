//! Git in a sandbox, with no credential in the sandbox.
//!
//! A project started from an SSH remote is a real clone inside the VM, with its
//! whole history, and every Git command works there except one kind: talking
//! to the remote. That needs the person's SSH key, and the key must not enter
//! the VM. So the daemon relays:
//!
//! ```text
//! VM:   git pull ─ ssh program is a relay script ─ fd 4 (exec stdout) ──┐
//!                                              ◄── fd 3 (exec stdin) ───┤
//! host: the daemon ── checks ── ssh with the person's agent ── the remote ─┘
//! ```
//!
//! The daemon starts Git in the VM through the engine's piped exec, with
//! `GIT_SSH_COMMAND` pointing at a small script. When Git "connects", the script
//! writes one header naming the Git service it wants, then joins Git's pipes to
//! the exec's stdin and stdout through two named pipes. The daemon reads the
//! header, starts the real `ssh` itself, to the project's own URL only, and
//! copies bytes both ways.
//!
//! The engine reports the end of the exec's stdout only when the exec ends, not
//! when the guest closes it (seen on msb 0.7.13). Real `ssh` closes the remote's
//! input when Git closes its side, and that is what lets the remote finish. With
//! no end to see, the remote would wait for another request while Git waits for
//! its `ssh` program to exit, for ever. So when Git closes its side, the script
//! sends a marker the daemon chose for this run, and the daemon closes `ssh`'s
//! input at that point in the stream. The marker is not a secret: a sandbox that
//! sends it early only ends its own connection.
//!
//! - **The repository is never the VM's choice.** The header's path is ignored;
//!   `ssh` is always started for the URL the project was imported from.
//! - **The service is checked against the operation.** A pull may only reach
//!   `git-upload-pack`, a push only `git-receive-pack`.
//! - **A push is read before any of it is sent.** Git's push protocol starts
//!   with a short list of branch changes, in pkt-lines, before the pack. The
//!   daemon reads that list, refuses a change to `main` or `master`, a delete,
//!   or anything outside `refs/heads/`, and only then forwards anything. A
//!   refused push sends the remote nothing at all.
//! - **The host parses nothing else.** After that list, and for every fetch,
//!   the bytes are copied without being read, so a hostile pack is never
//!   handled by Git on the host.
//!
//! A plain `git push` typed inside the VM finds a repository whose
//! `core.sshCommand` only says how to push from the host, and fails.
//!
//! What the daemon cannot see: a push that rewrites a branch's history looks
//! the same as one that adds to it, because telling them apart means reading
//! the pack. Branch protection on the remote can refuse those.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use engine::{ExecRequest, GuestPath};
use gitbroker::GitService;
use proto::text::TerminalFilter;
use proto::{ApiError, GitArgs, GitIdentity, GitOp, GitReport, RefUpdate};

use crate::{audit, bad, engine_error, internal, person, refused, remote, run, Progress, Project, Service};

/// Branches no relayed push may change. Branch protection on the remote is
/// still the stronger rule, because it holds whatever reaches the remote.
pub const PROTECTED_BRANCHES: &[&str] = &["main", "master"];

const WORK: &str = "/work";
const NETWORK_LIMIT: Duration = Duration::from_secs(15 * 60);
const LOCAL_LIMIT: Duration = Duration::from_secs(60);
/// A typed command that stays in the sandbox, such as a `gc` or a big `log`.
const RUN_LIMIT: Duration = Duration::from_secs(5 * 60);
const MAX_OUTPUT: usize = 64 * 1024;
const MAX_HEADER_LINE: u64 = 4096;
/// The push command list is small; this is far more than any real push needs.
const MAX_COMMANDS_BYTES: usize = 1024 * 1024;
const MAX_COMMIT_PATHS: usize = 1000;
#[cfg(test)]
const ZERO_SHA1: &str = "0000000000000000000000000000000000000000";

/// Who commits when the person's own name is not known.
const SANDBOX_NAME: &str = "Sandbox";
const SANDBOX_EMAIL: &str = "sandbox@localhost.invalid";

/// Runs Git in the VM with the relay script as its `ssh`. The arguments after
/// the script are Git's. Git's own output goes to stderr, so the exec's stdout
/// carries nothing but the relayed protocol. Two background `cat`s are the
/// only holders of the exec's stdin and stdout, joined to the relay by named
/// pipes, and both are stopped when Git ends, whether it connected or not.
const RELAYED_GIT: &str = r#"
command -v git >/dev/null 2>&1 || { echo "git is not installed in this sandbox" >&2; exit 127; }
d=$(mktemp -d) || exit 1
mkfifo "$d/in" "$d/out" || exit 1
cat > "$d/relay" <<'RELAY'
#!/bin/sh
d=$(dirname "$0")
for last; do :; done
cat "$d/in" &
{ printf 'git-relay 1\n%s\n%s\n' "${GIT_PROTOCOL:-}" "$last"; cat; printf '%s' "$GIT_RELAY_END"; } > "$d/out"
wait
RELAY
chmod 700 "$d/relay"
exec 3<&0 4>&1 </dev/null >&2
cat <&3 4>&- > "$d/in" & a=$!
cat "$d/out" 3<&- >&4 & b=$!
exec 3<&- 4>&-
GIT_SSH_COMMAND="$d/relay" GIT_SSH_VARIANT=ssh GIT_TERMINAL_PROMPT=0 git "$@"
s=$?
kill $a $b 2>/dev/null
rm -rf "$d"
exit $s
"#;

const HEADER: &str = "git-relay 1";
/// Tells the relay script what to send when Git closes its side.
const END_VAR: &str = "GIT_RELAY_END";

/// A marker for one run, unlike anything a pack could hold by chance. It starts
/// with a letter because `pump_to_end` holds back a partial match until the next
/// bytes come. Git ends each message it waits on with a newline or a `0000`
/// flush, so holding back a `g` can never stall a conversation.
fn end_marker() -> String {
    use std::hash::{BuildHasher, RandomState};
    format!(
        "git-relay-end-{:016x}{:016x}",
        RandomState::new().hash_one(1u8),
        RandomState::new().hash_one(2u8)
    )
}

/// What `core.sshCommand` says in the VM's clone, for a `git push` typed there.
fn ssh_stub() -> String {
    format!(
        "sh -c 'echo \"git push and pull cannot reach the remote from inside this sandbox, which holds no Git credentials, on purpose. Run them on your machine instead: {cli} git push, {cli} git pull (or the git_push and git_pull tools).\" >&2; exit 1'",
        cli = brand::CLI_BIN
    )
}

/// Answers a `git` request.
pub fn handle(
    service: &Service,
    project: &Project,
    args: &GitArgs,
    progress: &mut dyn Progress,
) -> Result<GitReport, ApiError> {
    let remote = remote::remote_of(service, project).ok_or_else(|| {
        bad(format!(
            "this project was not started from a Git remote, so there is nothing to pull from or push to. Start one with: {} up git@host:org/repo.git",
            brand::CLI_BIN
        ))
    })?;
    if run::status_of(service, project)? != proto::SandboxState::Running {
        return Err(crate::wrong_state(format!(
            "the sandbox is not running. Start it with: {} up {}",
            brand::CLI_BIN,
            remote.url
        )));
    }
    let identity = match &args.identity {
        Some(identity) => {
            proto::check_identity(identity).map_err(bad)?;
            identity.clone()
        }
        None => GitIdentity { name: SANDBOX_NAME.to_owned(), email: SANDBOX_EMAIL.to_owned() },
    };
    let agent = match &args.ssh_auth_sock {
        Some(path) => Some(remote::usable_agent_socket(Path::new(path)).ok_or_else(|| {
            bad("SSH_AUTH_SOCK does not name a socket that belongs to this user")
        })?),
        None => remote::last_agent(&remote.url),
    };
    let mut report = match &args.action {
        GitOp::Status => status(service, project)?,
        GitOp::Commit { message, paths } => commit(service, project, message, paths, &identity)?,
        GitOp::Pull => {
            let mut git = with_identity(&identity);
            git.extend(["pull", "--no-rebase", "--no-edit"].map(str::to_owned));
            let done = relay(service, project, &remote.url, git, GitService::UploadPack, agent.as_deref(), progress)?;
            service.record(
                audit::entry(service.deps().now_unix(), audit::Action::GitPulled)
                    .project(project.id.as_str())
                    .detail(if done.ok { "ok" } else { "failed" }),
            );
            done
        }
        GitOp::Push => {
            let git = ["push", "--set-upstream", "origin", "HEAD"].map(str::to_owned).to_vec();
            let done = relay(service, project, &remote.url, git, GitService::ReceivePack, agent.as_deref(), progress)?;
            record_push(service, project, &done);
            done
        }
        GitOp::Run { args } => run_typed(service, project, &remote.url, args, &identity, agent.as_deref(), progress)?,
    };
    report.branch = current_branch(service, project);
    Ok(report)
}

/// Clones a fetched remote into a new sandbox's `/work`, at the commit the
/// person approved, and leaves `git push` there pointing at the stub.
pub fn clone_into_sandbox(
    service: &Service,
    project: &Project,
    url: &str,
    git_ref: &str,
    commit: &str,
) -> Result<(), ApiError> {
    if !crate::gitref::is_commit_id(commit) {
        return Err(internal("there is no approved commit to clone at"));
    }
    let engine = service.deps().engine();
    engine
        .exec(&project.sandbox, &ExecRequest::new(["mkdir", "-p", WORK]))
        .map_err(|e| engine_error(e.to_string()))?;
    let mut git: Vec<String> = ["clone", "--no-checkout", "--origin", "origin"].map(str::to_owned).into();
    if git_ref != "HEAD" && !crate::gitref::is_commit_id(git_ref) {
        git.push("--branch".to_owned());
        git.push(git_ref.to_owned());
    }
    git.extend(["--", url, "."].map(str::to_owned));
    let agent = remote::last_agent(url);
    let done = relay(service, project, url, git, GitService::UploadPack, agent.as_deref(), &mut crate::Quiet)?;
    if !done.ok {
        service.record(
            audit::entry(service.deps().now_unix(), audit::Action::SandboxCloneFailed)
                .project(project.id.as_str())
                .subject(url)
                .detail(done.refused.clone().unwrap_or_else(|| last_line(&done.output))),
        );
        return Err(refused(format!("cloning into the sandbox failed:\n{}", done.output.trim_end())));
    }
    // At the approved commit, whatever the branch has gained since, and with
    // a plain `git push` in the VM pointed at a message rather than a key.
    let stub = ssh_stub();
    let mut finish = ExecRequest::new([
        "sh",
        "-c",
        r#"git reset -q --hard "$1" && git config core.sshCommand "$2""#,
        "sh",
        commit,
        &stub,
    ]);
    finish.workdir = Some(work()?);
    finish.timeout = Some(LOCAL_LIMIT);
    let out = engine.exec(&project.sandbox, &finish).map_err(|e| engine_error(e.to_string()))?;
    if out.code != 0 {
        return Err(internal(format!("could not check out the approved commit: {}", clean(out.stderr.as_bytes()))));
    }
    Ok(())
}

/// Runs a command typed by a person or an agent: inside the sandbox, with the
/// relay only for fetching and pushing (see `gitrun`). The person's name goes
/// on any commit or merge it makes.
fn run_typed(
    service: &Service,
    project: &Project,
    url: &str,
    args: &[String],
    identity: &GitIdentity,
    agent: Option<&Path>,
    progress: &mut dyn Progress,
) -> Result<GitReport, ApiError> {
    let route = crate::gitrun::classify(args).map_err(bad)?;
    let name = crate::gitrun::subcommand(args).to_owned();
    let mut git = with_identity(identity);
    git.extend(args.iter().cloned());
    let report = match route {
        crate::gitrun::Route::Relayed(allowed) => {
            let done = relay(service, project, url, git, allowed, agent, progress)?;
            if allowed == GitService::ReceivePack {
                record_push(service, project, &done);
            }
            done
        }
        crate::gitrun::Route::Local => {
            let mut command = vec!["git".to_owned()];
            command.extend(git);
            let mut request = ExecRequest::new(command);
            request.workdir = Some(work()?);
            request.timeout = Some(RUN_LIMIT);
            request.env.extend(no_prompts());
            let out = service
                .deps()
                .engine()
                .exec(&project.sandbox, &request)
                .map_err(|e| engine_error(e.to_string()))?;
            GitReport {
                ok: out.code == 0,
                code: Some(out.code),
                output: format!("{}{}", clean(out.stdout.as_bytes()), clean(out.stderr.as_bytes())),
                branch: None,
                updates: Vec::new(),
                refused: None,
            }
        }
    };
    service.record(
        audit::entry(service.deps().now_unix(), audit::Action::GitRan)
            .project(project.id.as_str())
            .subject(&name)
            .detail(match (&report.refused, report.ok) {
                (Some(_), _) => "refused",
                (None, true) => "ok",
                (None, false) => "failed",
            }),
    );
    Ok(report)
}

/// There is no terminal and no one to type: a command that would wait for an
/// editor, a pager or a password fails at once and says why.
fn no_prompts() -> Vec<(String, String)> {
    let editor = "sh -c 'echo \"There is no editor here. Give the message with -m, or use --no-edit.\" >&2; exit 1'";
    [
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_PAGER", "cat"),
        ("PAGER", "cat"),
        ("GIT_EDITOR", editor),
        ("GIT_SEQUENCE_EDITOR", editor),
        ("GIT_MERGE_AUTOEDIT", "no"),
    ]
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .to_vec()
}

fn work() -> Result<GuestPath, ApiError> {
    GuestPath::new(WORK).map_err(|e| internal(e.to_string()))
}

fn with_identity(identity: &GitIdentity) -> Vec<String> {
    vec![
        "-c".to_owned(),
        format!("user.name={}", identity.name),
        "-c".to_owned(),
        format!("user.email={}", identity.email),
    ]
}

/// The last line with anything on it, which is where Git says what went wrong.
fn last_line(output: &str) -> String {
    output.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("no output").trim().to_owned()
}

fn clean(bytes: &[u8]) -> String {
    let mut filter = TerminalFilter::new();
    let mut text = filter.push(&bytes[..bytes.len().min(MAX_OUTPUT)]);
    text.push_str(&filter.finish());
    if bytes.len() > MAX_OUTPUT {
        text.push_str("\n[cut off]\n");
    }
    text
}

fn current_branch(service: &Service, project: &Project) -> Option<String> {
    let mut request = ExecRequest::new(["git", "rev-parse", "--abbrev-ref", "HEAD"]);
    request.workdir = work().ok();
    request.timeout = Some(LOCAL_LIMIT);
    let out = service.deps().engine().exec(&project.sandbox, &request).ok()?;
    let branch = out.stdout.trim();
    (out.code == 0 && !branch.is_empty() && branch.len() < 256 && branch != "HEAD")
        .then(|| proto::text::escape(branch))
}

fn status(service: &Service, project: &Project) -> Result<GitReport, ApiError> {
    let mut request = ExecRequest::new(["git", "status", "--short", "--branch"]);
    request.workdir = Some(work()?);
    request.timeout = Some(LOCAL_LIMIT);
    let out = service
        .deps()
        .engine()
        .exec(&project.sandbox, &request)
        .map_err(|e| engine_error(e.to_string()))?;
    Ok(GitReport {
        ok: out.code == 0,
        code: Some(out.code),
        output: format!("{}{}", clean(out.stdout.as_bytes()), clean(out.stderr.as_bytes())),
        branch: None,
        updates: Vec::new(),
        refused: None,
    })
}

/// A path to stage, relative to `/work`, kept off Git's options.
fn check_path(path: &str) -> Result<(), ApiError> {
    let ok = !path.is_empty()
        && path.len() <= 4096
        && !path.starts_with(['/', '-'])
        && !path.chars().any(char::is_control)
        && !path.split('/').any(|part| part == ".." || part.is_empty());
    if ok {
        Ok(())
    } else {
        Err(bad("a path to commit must be relative to the project, with no '..', and must not start with '-'"))
    }
}

fn commit(
    service: &Service,
    project: &Project,
    message: &str,
    paths: &[String],
    identity: &GitIdentity,
) -> Result<GitReport, ApiError> {
    if message.trim().is_empty() {
        return Err(bad("a commit needs a message"));
    }
    if message.len() > proto::MAX_COMMIT_MESSAGE_BYTES || message.contains('\0') {
        return Err(bad("that commit message is too long or holds a NUL"));
    }
    if paths.len() > MAX_COMMIT_PATHS {
        return Err(bad("too many paths for one commit"));
    }
    for path in paths {
        check_path(path)?;
    }
    // The message goes in on stdin and the paths as arguments, so neither can
    // change what the script does.
    let mut command: Vec<String> = vec![
        "sh".to_owned(),
        "-c".to_owned(),
        r#"n="$1"; e="$2"; shift 2; if [ "$#" -eq 0 ]; then git add -A; else git add -A -- "$@"; fi && git -c user.name="$n" -c user.email="$e" commit -q -F - && git log -1 --oneline"#.to_owned(),
        "sh".to_owned(),
        identity.name.clone(),
        identity.email.clone(),
    ];
    command.extend(paths.iter().cloned());
    let mut request = ExecRequest::new(command);
    request.workdir = Some(work()?);
    request.timeout = Some(LOCAL_LIMIT);
    let out = service
        .deps()
        .engine()
        .exec_with_input(&project.sandbox, &request, message.as_bytes())
        .map_err(|e| engine_error(e.to_string()))?;
    Ok(GitReport {
        ok: out.code == 0,
        code: Some(out.code),
        output: format!("{}{}", clean(out.stdout.as_bytes()), clean(out.stderr.as_bytes())),
        branch: None,
        updates: Vec::new(),
        refused: None,
    })
}

fn record_push(service: &Service, project: &Project, report: &GitReport) {
    let now = service.deps().now_unix();
    if let Some(reason) = &report.refused {
        service.record(
            audit::entry(now, audit::Action::GitPushRefused)
                .project(project.id.as_str())
                .detail(reason.clone()),
        );
        return;
    }
    for update in &report.updates {
        service.record(
            audit::entry(now, audit::Action::GitPushed)
                .project(project.id.as_str())
                .subject(&update.name)
                .detail(format!("{} to {}{}", update.old, update.new, if report.ok { "" } else { ", not confirmed" })),
        );
    }
}

// ---- the relay

/// Runs one Git command in the VM, relaying its one connection to the remote.
fn relay(
    service: &Service,
    project: &Project,
    url: &str,
    git: Vec<String>,
    allowed: GitService,
    agent: Option<&Path>,
    progress: &mut dyn Progress,
) -> Result<GitReport, ApiError> {
    let mut command: Vec<String> = vec!["sh".to_owned(), "-c".to_owned(), RELAYED_GIT.to_owned(), "relayed-git".to_owned()];
    command.extend(git);
    let mut request = ExecRequest::new(command);
    request.workdir = Some(work()?);
    let end = end_marker();
    request.env.push((END_VAR.to_owned(), end.clone()));
    request.env.extend(no_prompts());
    let mut vm = service
        .deps()
        .engine()
        .exec_piped(&project.sandbox, &request)
        .map_err(|e| engine_error(e.to_string()))?;
    let deadline = Instant::now() + NETWORK_LIMIT;

    let vm_err = collect(vm.stderr.take());
    let (Some(vm_in), Some(vm_out)) = (vm.stdin.take(), vm.stdout.take()) else {
        let _ = vm.kill();
        return Err(internal("the sandbox's pipes were not created"));
    };
    let mut vm_out = BufReader::new(vm_out);

    let mut refusal: Option<String> = None;
    let mut updates = Vec::new();
    let mut nothing_to_push = false;
    let mut ssh_err: Option<Arc<Mutex<Vec<u8>>>> = None;

    match read_header(&mut vm_out) {
        // Git ended without connecting: an error before the network, or
        // nothing to do. Its own words say which.
        Ok(None) => drop(vm_in),
        Err(reason) => {
            refusal = Some(reason);
            drop(vm_in);
        }
        Ok(Some(header)) if header.service != allowed => {
            refusal = Some(format!(
                "this operation may only use {}, and Git asked for {}",
                allowed.program(),
                header.service.program()
            ));
            drop(vm_in);
        }
        Ok(Some(header)) => {
            match service.deps().spawn_ssh(url, header.service, header.git_protocol.as_deref(), agent) {
                Err(reason) => {
                    refusal = Some(remote::explain_ssh(&reason, url));
                    drop(vm_in);
                }
                Ok(mut child) => {
                    ssh_err = Some(collect(child.stderr.take()));
                    let to_vm = child.stdout.take().map(|out| pump(out, vm_in));
                    let mut ssh_in = child.stdin.take();
                    let mut ssh = Some(child);
                    // A push's branch list is read in full and checked before
                    // a byte of it is sent on.
                    if header.service == GitService::ReceivePack {
                        match read_commands(&mut vm_out) {
                            Ok((raw, commands)) => match check_commands(&commands) {
                                // Git found nothing new to push. It sends an
                                // empty list and the remote changes nothing,
                                // so there is nothing to ask the person. Only
                                // that list goes on; whatever might follow it
                                // is not sent.
                                Ok(found) if found.is_empty() => {
                                    nothing_to_push = true;
                                    if let Some(mut input) = ssh_in.take() {
                                        let _ = input.write_all(&raw).and_then(|()| input.flush());
                                    }
                                }
                                // The person sees the exact branches and
                                // commits, and nothing reaches the remote
                                // until they confirm.
                                Ok(found) => match service.ask_person(
                                    project,
                                    person::Act::Push,
                                    &push_reason(project, url, &found),
                                    progress,
                                ) {
                                    Ok(()) => {
                                        updates = found;
                                        if let Some(input) = ssh_in.as_mut() {
                                            let _ = input.write_all(&raw).and_then(|()| input.flush());
                                        }
                                    }
                                    Err(error) => refusal = Some(error.message),
                                },
                                Err(reason) => refusal = Some(reason),
                            },
                            Err(reason) => refusal = Some(reason),
                        }
                    }
                    if refusal.is_none() {
                        if let Some(input) = ssh_in.take() {
                            let to_remote = pump_to_end(vm_out, input, end.into_bytes());
                            wait_for(&mut vm, deadline);
                            let _ = to_remote.join();
                        }
                    }
                    if let Some(ssh) = ssh.as_mut() {
                        if refusal.is_some() {
                            let _ = ssh.kill();
                        }
                        wait_for(ssh, Instant::now() + Duration::from_secs(10));
                    }
                    if let Some(to_vm) = to_vm {
                        let _ = to_vm.join();
                    }
                }
            }
        }
    }
    if refusal.is_some() {
        let _ = vm.kill();
    }
    let status = wait_for(&mut vm, deadline);
    let timed_out = status.is_none();
    let code = status.and_then(|s| s.code());

    let mut output = clean(&take(&vm_err));
    if let Some(err) = &ssh_err {
        let text = clean(&take(err));
        if !text.trim().is_empty() {
            output.push_str(&remote::explain_ssh(text.trim_end(), url));
            output.push('\n');
        }
    }
    if nothing_to_push {
        output.push_str("Nothing needs pushing, so you were not asked to confirm.\n");
    }
    if timed_out {
        output.push_str(&format!("\nstopped after {} minutes\n", NETWORK_LIMIT.as_secs() / 60));
    }
    if let Some(reason) = &refusal {
        output.push_str(&format!("\nrefused by {}: {reason}\n", brand::DAEMON_BIN));
    }
    Ok(GitReport {
        ok: refusal.is_none() && code == Some(0),
        code,
        output,
        branch: None,
        updates,
        refused: refusal,
    })
}

/// "push my-change (1f2a120 to 9c3d4e5) from shop to git@github.com:org/shop.git".
fn push_reason(project: &Project, url: &str, updates: &[RefUpdate]) -> String {
    let short = |id: &str| id.get(..7).unwrap_or(id).to_owned();
    let changes = updates
        .iter()
        .map(|u| {
            let branch = u.name.strip_prefix("refs/heads/").unwrap_or(&u.name);
            if u.old.bytes().all(|b| b == b'0') {
                format!("new branch {branch} at {}", short(&u.new))
            } else {
                format!("{branch} ({} to {})", short(&u.old), short(&u.new))
            }
        })
        .collect::<Vec<_>>();
    let changes = if changes.is_empty() { "nothing".to_owned() } else { changes.join(", ") };
    format!("push {changes} from {} to {url}", person::project_name(project))
}

struct RelayHeader {
    service: GitService,
    git_protocol: Option<String>,
}

fn read_line(reader: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    let read = reader.take(MAX_HEADER_LINE).read_until(b'\n', &mut line)?;
    if read == 0 {
        return Ok(None);
    }
    if line.pop() != Some(b'\n') {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "a header line was too long"));
    }
    Ok(Some(String::from_utf8_lossy(&line).into_owned()))
}

/// Reads the relay script's header. `None` means Git never connected.
fn read_header(reader: &mut impl BufRead) -> Result<Option<RelayHeader>, String> {
    let bad = |_| "the relay's header could not be read".to_owned();
    let Some(first) = read_line(reader).map_err(bad)? else {
        return Ok(None);
    };
    if first != HEADER {
        return Err("the sandbox did not start the relay as expected".to_owned());
    }
    let protocol = read_line(reader).map_err(bad)?.unwrap_or_default();
    let command = read_line(reader).map_err(bad)?.unwrap_or_default();
    let service = GitService::from_remote_command(&command)
        .ok_or_else(|| "Git asked for something other than fetching or pushing".to_owned())?;
    let git_protocol = gitbroker::valid_git_protocol(&protocol).then_some(protocol);
    Ok(Some(RelayHeader { service, git_protocol }))
}

/// One line of a push's command list.
#[derive(Debug, PartialEq, Eq)]
enum PushCommand {
    Update { old: String, new: String, name: String },
    Shallow,
    Other(String),
}

fn is_object_id(text: &str) -> bool {
    (text.len() == 40 || text.len() == 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Reads pkt-lines up to the first flush, returning the raw bytes to forward
/// and what they asked for. Nothing is forwarded until this returns.
fn read_commands(reader: &mut impl BufRead) -> Result<(Vec<u8>, Vec<PushCommand>), String> {
    let mut raw = Vec::new();
    let mut commands = Vec::new();
    loop {
        let mut size = [0u8; 4];
        if reader.read_exact(&mut size).is_err() {
            // Git closed without pushing anything, which it does when there
            // is nothing to send.
            return if raw.is_empty() { Ok((raw, commands)) } else { Err("the push ended part way through its branch list".to_owned()) };
        }
        raw.extend_from_slice(&size);
        let size = std::str::from_utf8(&size)
            .ok()
            .and_then(|hex| usize::from_str_radix(hex, 16).ok())
            .ok_or("the push's branch list was not in Git's format")?;
        if size == 0 {
            return Ok((raw, commands));
        }
        if size < 4 || raw.len() + size > MAX_COMMANDS_BYTES {
            return Err("the push's branch list was not in Git's format".to_owned());
        }
        let mut payload = vec![0u8; size - 4];
        reader
            .read_exact(&mut payload)
            .map_err(|_| "the push ended part way through its branch list".to_owned())?;
        raw.extend_from_slice(&payload);
        // The first line carries capabilities after a NUL.
        let mut parts = payload.splitn(2, |b| *b == 0);
        let line = parts.next().unwrap_or(&[]);
        let line = String::from_utf8_lossy(line);
        let line = line.trim_end_matches('\n');
        commands.push(parse_command(line));
        // Push options follow the list and would be passed on unread, for the
        // remote to act on. None are allowed.
        if let Some(caps) = parts.next() {
            if String::from_utf8_lossy(caps).split_whitespace().any(|cap| cap == "push-options") {
                commands.push(PushCommand::Other("push options".to_owned()));
            }
        }
    }
}

fn parse_command(line: &str) -> PushCommand {
    if line.starts_with("shallow ") {
        return PushCommand::Shallow;
    }
    let mut parts = line.splitn(3, ' ');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(old), Some(new), Some(name)) if is_object_id(old) && is_object_id(new) => PushCommand::Update {
            old: old.to_ascii_lowercase(),
            new: new.to_ascii_lowercase(),
            name: name.to_owned(),
        },
        _ => PushCommand::Other(proto::text::escape_short(line, 200)),
    }
}

/// The push rules. Every update must pass, or none is sent.
fn check_commands(commands: &[PushCommand]) -> Result<Vec<RefUpdate>, String> {
    let mut updates = Vec::new();
    for command in commands {
        match command {
            PushCommand::Shallow => {}
            PushCommand::Other(text) => {
                return Err(format!("the push held something this relay does not allow: {text}"));
            }
            PushCommand::Update { old, new, name } => {
                let shown = proto::text::escape_short(name, 200);
                let Some(branch) = name.strip_prefix("refs/heads/") else {
                    return Err(format!("only branches can be pushed, not {shown}"));
                };
                if PROTECTED_BRANCHES.contains(&branch) {
                    return Err(format!(
                        "pushing to {branch} is not allowed from a sandbox. Make a branch with git switch -c NAME, then push that"
                    ));
                }
                if new.bytes().all(|b| b == b'0') {
                    return Err(format!("deleting a branch from a sandbox is not allowed: {shown}"));
                }
                updates.push(RefUpdate { name: shown, old: old.clone(), new: new.clone() });
            }
        }
    }
    Ok(updates)
}

fn pump(mut from: impl Read + Send + 'static, mut to: impl Write + Send + 'static) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0u8; 64 * 1024];
        loop {
            match from.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if to.write_all(&buffer[..read]).and_then(|()| to.flush()).is_err() {
                        break;
                    }
                }
            }
        }
    })
}

/// Copies like `pump`, and closes `to` at the marker, or at the end of `from` if
/// the marker never comes. `from` is still read to its end after the marker, so
/// the engine never writes to a closed pipe.
fn pump_to_end(
    mut from: impl Read + Send + 'static,
    to: impl Write + Send + 'static,
    end: Vec<u8>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut to = Some(to);
        let mut held: Vec<u8> = Vec::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = match from.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            let Some(out) = to.as_mut() else { continue };
            held.extend_from_slice(&buffer[..read]);
            let found = held.windows(end.len()).position(|window| window == end);
            let send = found.unwrap_or_else(|| held.len() - partial_match(&held, &end));
            let sent = out.write_all(&held[..send]).and_then(|()| out.flush());
            if found.is_some() || sent.is_err() {
                to = None;
                held = Vec::new();
            } else {
                held.drain(..send);
            }
        }
        if let Some(mut out) = to {
            let _ = out.write_all(&held);
        }
    })
}

/// How many bytes at the end of `data` could be the start of `end`.
fn partial_match(data: &[u8], end: &[u8]) -> usize {
    (1..end.len().min(data.len() + 1))
        .rev()
        .find(|&length| data.ends_with(&end[..length]))
        .unwrap_or(0)
}

fn collect(pipe: Option<impl Read + Send + 'static>) -> Arc<Mutex<Vec<u8>>> {
    let kept = Arc::new(Mutex::new(Vec::new()));
    if let Some(mut pipe) = pipe {
        let into = Arc::clone(&kept);
        thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            while let Ok(read) = pipe.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                let mut kept = into.lock().unwrap_or_else(|e| e.into_inner());
                let room = (MAX_OUTPUT + 1).saturating_sub(kept.len());
                kept.extend_from_slice(&buffer[..read.min(room)]);
            }
        });
    }
    kept
}

fn take(kept: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    // The reader may still be finishing; a moment lets the last words arrive.
    thread::sleep(Duration::from_millis(50));
    std::mem::take(&mut *kept.lock().unwrap_or_else(|e| e.into_inner()))
}

fn wait_for(child: &mut Child, deadline: Instant) -> Option<std::process::ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(line: &str) -> Vec<u8> {
        let mut out = format!("{:04x}", line.len() + 4).into_bytes();
        out.extend_from_slice(line.as_bytes());
        out
    }

    fn list(lines: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for line in lines {
            out.extend(pkt(line));
        }
        out.extend_from_slice(b"0000PACK...");
        out
    }

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    fn check(lines: &[&str]) -> Result<Vec<RefUpdate>, String> {
        let bytes = list(lines);
        let mut reader = io::Cursor::new(bytes.clone());
        let (raw, commands) = read_commands(&mut reader).unwrap();
        // Only the list is consumed; the pack stays for the plain copy.
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"PACK...");
        assert_eq!([raw, rest].concat(), bytes);
        check_commands(&commands)
    }

    #[test]
    fn a_push_to_a_feature_branch_is_allowed_and_reported() {
        let first = format!("{A} {B} refs/heads/feature/x\0report-status side-band-64k\n");
        let updates = check(&[&first]).unwrap();
        assert_eq!(updates, [RefUpdate { name: "refs/heads/feature/x".into(), old: A.into(), new: B.into() }]);
        let created = format!("{ZERO_SHA1} {B} refs/heads/new\n");
        assert_eq!(check(&[&created]).unwrap().len(), 1);
    }

    #[test]
    fn protected_branches_deletes_tags_and_odd_lines_are_refused() {
        for line in [
            format!("{A} {B} refs/heads/main\0caps\n"),
            format!("{A} {B} refs/heads/master\n"),
            format!("{A} {ZERO_SHA1} refs/heads/feature\n"),
            format!("{A} {B} refs/tags/v1\n"),
            format!("{A} {B} refs/notes/commits\n"),
            "push-cert\0caps\n".to_owned(),
            "garbage\n".to_owned(),
        ] {
            assert!(check(&[&line]).is_err(), "{line:?}");
        }
        // One bad update stops all of them.
        let good = format!("{A} {B} refs/heads/feature\0caps\n");
        let bad = format!("{A} {B} refs/heads/main\n");
        assert!(check(&[&good, &bad]).is_err());
    }

    #[test]
    fn push_options_are_refused_and_other_capabilities_are_not() {
        let with = format!("{A} {B} refs/heads/f\0report-status push-options side-band-64k\n");
        assert!(check(&[&with]).unwrap_err().contains("push options"));
        let without = format!("{A} {B} refs/heads/f\0report-status side-band-64k\n");
        assert_eq!(check(&[&without]).unwrap().len(), 1);
    }

    #[test]
    fn shallow_lines_are_allowed() {
        let lines = [format!("shallow {A}\n"), format!("{A} {B} refs/heads/f\0caps\n")];
        assert_eq!(check(&[&lines[0], &lines[1]]).unwrap().len(), 1);
    }

    #[test]
    fn a_broken_or_endless_list_is_refused() {
        let mut reader = io::Cursor::new(b"zzzz".to_vec());
        assert!(read_commands(&mut reader).is_err());
        let mut reader = io::Cursor::new(b"0003".to_vec());
        assert!(read_commands(&mut reader).is_err());
        let mut reader = io::Cursor::new(b"00ffshort".to_vec());
        assert!(read_commands(&mut reader).is_err());
        let mut reader = io::Cursor::new(Vec::new());
        assert_eq!(read_commands(&mut reader).unwrap().1, []);
    }

    #[test]
    fn the_header_names_the_service_and_nothing_else_counts() {
        let mut reader = io::Cursor::new(b"git-relay 1\nversion=2\ngit-upload-pack 'evil/other.git'\n".to_vec());
        let header = read_header(&mut reader).unwrap().unwrap();
        assert_eq!(header.service, GitService::UploadPack);
        assert_eq!(header.git_protocol.as_deref(), Some("version=2"));
        let mut reader = io::Cursor::new(b"git-relay 1\nversion=9; x\ngit-receive-pack 'r'\n".to_vec());
        assert_eq!(read_header(&mut reader).unwrap().unwrap().git_protocol, None);
        let mut reader = io::Cursor::new(b"hello\n".to_vec());
        assert!(read_header(&mut reader).is_err());
        let mut reader = io::Cursor::new(b"git-relay 1\n\nsh -c id\n".to_vec());
        assert!(read_header(&mut reader).is_err());
        assert!(read_header(&mut io::Cursor::new(Vec::new())).unwrap().is_none());
    }

    #[test]
    fn commit_paths_stay_inside_the_project_and_off_git_options() {
        for good in ["src/a.rs", "a b", ".github/x"] {
            assert!(check_path(good).is_ok(), "{good}");
        }
        for bad in ["", "/etc", "-p", "../x", "a/../b", "a//b", "a\nb"] {
            assert!(check_path(bad).is_err(), "{bad:?}");
        }
    }

    /// Runs the relay script on this machine, with a stub in place of `git`.
    /// The caller holds the exec's stdin and stdout, as the engine does.
    fn start_relayed(stub: &str, marker: &str) -> (tempfile::TempDir, Child) {
        use std::os::unix::fs::PermissionsExt;
        let bin = tempfile::tempdir().unwrap();
        let git = bin.path().join("git");
        std::fs::write(&git, format!("#!/bin/sh\n{stub}\n")).unwrap();
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
        let child = std::process::Command::new("sh")
            .args(["-c", RELAYED_GIT, "relayed-git"])
            .env("PATH", format!("{}:/usr/bin:/bin", bin.path().display()))
            .env(END_VAR, marker)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        (bin, child)
    }

    /// The engine does not report the end of the exec's stdout until the exec
    /// ends, so the marker is the only news the host gets that Git has closed its
    /// side. It has to arrive while the host is still holding everything open,
    /// and Git must still get what the server sends afterwards: Git closes its
    /// side before it reads a pack or a push's report.
    #[test]
    fn the_script_sends_the_marker_when_git_closes_and_still_delivers_the_reply() {
        let (_bin, mut child) =
            start_relayed(r#"$GIT_SSH_COMMAND git@host "git-upload-pack 'repo'" </dev/null"#, "the-marker");
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        assert_eq!(read_line(&mut stdout).unwrap().as_deref(), Some(HEADER));
        read_line(&mut stdout).unwrap();
        read_line(&mut stdout).unwrap();
        let mut marker = [0u8; 10];
        stdout.read_exact(&mut marker).unwrap();
        assert_eq!(&marker, b"the-marker");

        stdin.write_all(b"the-reply\n").unwrap();
        drop(stdin);
        let status = wait_for(&mut child, Instant::now() + Duration::from_secs(10));
        assert!(status.is_some_and(|s| s.success()));
        let mut seen = String::new();
        child.stderr.take().unwrap().read_to_string(&mut seen).unwrap();
        assert!(seen.contains("the-reply"), "{seen:?}");
    }

    #[test]
    fn the_pump_closes_at_the_marker_while_the_stream_stays_open() {
        let (from, mut writer) = io::pipe().unwrap();
        let (mut result, to) = io::pipe().unwrap();
        let pump = pump_to_end(from, to, b"END!".to_vec());
        // The marker split across two reads, which must not lose or leak any of it.
        writer.write_all(b"abcEN").unwrap();
        writer.flush().unwrap();
        thread::sleep(Duration::from_millis(50));
        writer.write_all(b"D!ignored").unwrap();
        // Reading to the end proves `to` closed with `writer` still open.
        let mut seen = String::new();
        result.read_to_string(&mut seen).unwrap();
        assert_eq!(seen, "abc");
        drop(writer);
        pump.join().unwrap();
    }

    #[test]
    fn the_pump_passes_everything_on_at_once_and_closes_if_no_marker_comes() {
        let (from, mut writer) = io::pipe().unwrap();
        let (result, to) = io::pipe().unwrap();
        let pump = pump_to_end(from, to, end_marker().into_bytes());
        let mut result = BufReader::new(result);
        // A request must not sit in the pump: the other side answers it first.
        writer.write_all(b"0009done\n").unwrap();
        assert_eq!(read_line(&mut result).unwrap().as_deref(), Some("0009done"));
        // A partial match is passed on once it turns out not to be the marker.
        writer.write_all(b"git-rel").unwrap();
        writer.flush().unwrap();
        thread::sleep(Duration::from_millis(50));
        writer.write_all(b"ay\n").unwrap();
        assert_eq!(read_line(&mut result).unwrap().as_deref(), Some("git-relay"));
        writer.write_all(b"g").unwrap();
        drop(writer);
        pump.join().unwrap();
        let mut rest = String::new();
        result.read_to_string(&mut rest).unwrap();
        assert_eq!(rest, "g");
    }

    #[test]
    fn a_partial_match_is_the_longest_end_of_the_data_that_starts_the_marker() {
        assert_eq!(partial_match(b"abc", b"END!"), 0);
        assert_eq!(partial_match(b"abcE", b"END!"), 1);
        assert_eq!(partial_match(b"abcEND", b"END!"), 3);
        assert_eq!(partial_match(b"", b"END!"), 0);
    }

    #[test]
    fn each_run_gets_its_own_marker() {
        assert_ne!(end_marker(), end_marker());
    }
}
