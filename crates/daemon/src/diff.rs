//! What changed inside a sandbox, compared with the snapshot it was given.
//!
//! # The baseline
//!
//! A snapshot is a plain folder with no history. When a sandbox is created,
//! the host commits that folder itself ([`gitbroker::make_baseline`]): the
//! commit goes into the guest as `/work/.git`, and a copy stays in a bare
//! repository in this project's state folder. Its id is kept in the sandbox
//! record. All of that happens before the guest has run anything from the
//! project, and none of it is ever read back from the guest.
//!
//! # Getting the changes out
//!
//! Git runs inside the guest only. It commits the whole of `/work` onto a
//! throwaway branch, using a temporary index so the guest's own index and
//! branches are left alone, and writes a bundle of that branch without the
//! baseline's objects. The bundle comes back as base64 on the command's
//! output, through the engine's streamed exec:
//!
//! - nothing is written on the host by the engine, so no path or link the
//!   guest made can become one on the host;
//! - the size limit is enforced on the bytes as they arrive, and the command
//!   is stopped the moment they pass it;
//! - anything that is not base64 ends the read.
//!
//! The host then treats the bundle as hostile data: [`gitbroker::Quarantine`]
//! checks its header and refs, fetches it with fsck into a new bare repository
//! made for this one comparison, and reviews it against the host's own copy
//! of the baseline. Host git never runs in a `.git` the guest wrote, and
//! nothing from the bundle is checked out or run.
//!
//! What the guest says it changed can still be a lie: it can leave a file out
//! of the bundle. Such a file is then not in anything an export would carry
//! either, since an export would be made from the same reviewed commit.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use engine::{ExecRequest, GuestPath, OutputSink, OutputStream, StreamEnd};
use gitbroker::{ChangeStatus, Flag, FlagKind, LineKind, Patch, PatchLimits, Quarantine, Review};
use proto::{
    text, ApiError, DiffFile, DiffFlag, DiffHunk, DiffLine, DiffLineKind, DiffReport, FilePatch,
    FileChange, SandboxState, MAX_FRAME_BYTES,
};

use crate::run::{status_of, tempdir_in_state, WORK_DIR};
use crate::settings::SandboxRecord;
use crate::{audit, engine_error, internal, refused, wrong_state, Progress, Project, Service};

/// The host's copy of the baseline, inside the project's state folder.
pub const BASELINE_STORE: &str = "baseline.git";
/// The one branch a bundle from the guest may hold.
pub const TIP_REF: &str = "refs/heads/sandbox-review";
/// The largest bundle taken from a guest, after decoding.
pub const MAX_BUNDLE_BYTES: u64 = 128 * 1024 * 1024;
/// How long the guest may take to write its bundle.
const BUNDLE_LIMIT: Duration = Duration::from_secs(10 * 60);
const MAX_STDERR: usize = 16 * 1024;
const MAX_PATH_CHARS: usize = 400;
const MAX_FILES: usize = 20_000;
const MAX_FLAGS: usize = 2_000;
/// Files whose lines are read in one comparison.
const MAX_PATCHED_FILES: usize = 2_000;
/// How long reading those lines may take. The rest are then not included.
const PATCH_TIME_LIMIT: Duration = Duration::from_secs(30);
/// One file's share of a message, so one file cannot use up all of it.
const MAX_FILE_PATCH_BYTES: usize = 256 * 1024;
/// Kept free in a message for what wraps the report.
const FRAME_MARGIN: usize = 4 * 1024;
/// What one line costs in a message besides its text.
const LINE_OVERHEAD: usize = 32;
const MAX_LINE_CHARS: usize = 2_000;
const NO_NEWLINE: &str = "  (no newline at end of file)";

/// Run inside the guest by `sh -c`, with the baseline's id as `$1`. It has no
/// say in what the host trusts: the host checks everything it sends.
///
/// - `safe.directory`: the copied files may belong to another user in the guest.
/// - `core.ignorecase=false` and `core.precomposeunicode=false`: a snapshot made
///   on a Mac carried `core.ignorecase=true`, and then two paths that differ
///   only in case, such as `.claude/settings.json` and `.CLAUDE/Settings.json`,
///   stopped `add` with "will not add file alias". The guest's disk tells case
///   apart, so git must too. Given on the command line, they win over anything
///   the guest put in `/work/.git/config`, and the system and global config
///   files are not read.
/// - A temporary index, so the guest's own staged state is not touched.
/// - `add --all` follows the project's ignore files, as a commit would, so an
///   ignored file is neither shown nor in anything an export would carry.
/// - Without the baseline in its history, the whole history is bundled, and
///   the host reports that the comparison may not show everything.
const GUEST_SCRIPT: &str = r#"set -eu
base="$1"
cd /work
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
g() {
  git -c safe.directory=/work -c core.hooksPath=/dev/null -c core.fsmonitor=false \
      -c core.ignorecase=false -c core.precomposeunicode=false \
      -c commit.gpgSign=false -c user.name=sandbox -c user.email=sandbox@localhost.invalid "$@"
}
if ! g rev-parse --verify --quiet 'HEAD^{commit}' >/dev/null; then
  echo "/work has no commit to compare with" >&2
  exit 3
fi
GIT_INDEX_FILE="$t/index" g read-tree HEAD
GIT_INDEX_FILE="$t/index" g add --all
tree=$(GIT_INDEX_FILE="$t/index" g write-tree)
tip=$(g commit-tree "$tree" -p HEAD -m "Work in the sandbox, including uncommitted changes")
g update-ref refs/heads/sandbox-review "$tip"
if g cat-file -e "$base^{commit}" 2>/dev/null; then
  g bundle create -q "$t/out.bundle" refs/heads/sandbox-review "^$base"
else
  g bundle create -q "$t/out.bundle" refs/heads/sandbox-review
fi
base64 <"$t/out.bundle"
"#;

/// Makes the baseline in the host's snapshot copy, just before it is copied
/// in. A failure is recorded and is not fatal: the sandbox can still run, and
/// comparing its changes is refused later with the reason.
pub(crate) fn make_baseline(
    service: &Service,
    project: &Project,
    snapshot: &Path,
) -> Option<String> {
    forget_baseline(project);
    let store = project.dir.join(BASELINE_STORE);
    let dot_git = snapshot.join(".git");
    let had_git = std::fs::symlink_metadata(&dot_git).is_ok();
    match gitbroker::make_baseline(snapshot, &store) {
        Ok(commit) => {
            service.record(
                audit::entry(service.deps().now_unix(), audit::Action::BaselineMade)
                    .project(project.id.as_str())
                    .subject(&commit),
            );
            Some(commit)
        }
        Err(error) => {
            // Half a repository must not reach the guest as if it were one.
            // Only one this made is removed: the staging copy is the host's.
            if !had_git {
                let _ = std::fs::remove_dir_all(&dot_git);
            }
            forget_baseline(project);
            service.record(
                audit::entry(service.deps().now_unix(), audit::Action::BaselineMissing)
                    .project(project.id.as_str())
                    .detail(error.to_string()),
            );
            None
        }
    }
}

/// Notes the baseline in the sandbox record, once the snapshot is in.
pub(crate) fn remember_baseline(
    project: &Project,
    baseline: Option<String>,
) -> Result<(), ApiError> {
    let Some(mut record) = SandboxRecord::load(&project.dir).map_err(internal)? else {
        return Ok(());
    };
    record.baseline = baseline;
    record.save(&project.dir).map_err(internal)
}

/// Removes the host's copy of a baseline. It was made by the host from the
/// host's own staging copy, so removing it follows nothing the guest made.
pub(crate) fn forget_baseline(project: &Project) {
    let store = project.dir.join(BASELINE_STORE);
    if std::fs::symlink_metadata(&store).is_ok_and(|m| m.is_dir()) {
        let _ = std::fs::remove_dir_all(&store);
    }
}

/// What every failure to get or read the sandbox's changes starts with. Code in
/// the sandbox can make the review fail on purpose, so a failure must never
/// read as "nothing to see".
pub const NOT_REVIEWED: &str =
    "the review could not run, so treat everything the sandbox changed as unreviewed";

fn not_reviewed(mut error: ApiError) -> ApiError {
    error.message = format!("{NOT_REVIEWED}: {}", error.message);
    error
}

fn no_baseline() -> ApiError {
    wrong_state(format!(
        "there is no record of what this sandbox started from, so its changes cannot be compared. It was created before this was kept, or the record could not be made. To compare, recreate it with {bin} down --destroy, then {bin} up, which deletes its data.",
        bin = brand::CLI_BIN
    ))
}

pub fn diff(
    service: &Service,
    project: &Project,
    progress: &mut dyn Progress,
) -> Result<DiffReport, ApiError> {
    match status_of(service, project)? {
        SandboxState::Running => {}
        SandboxState::None => {
            return Err(wrong_state(format!(
                "this project has no sandbox, so there is nothing to compare. Start it with {} up.",
                brand::CLI_BIN
            )))
        }
        _ => {
            return Err(wrong_state(format!(
                "the sandbox is not running, and the comparison runs inside it. Start it with {} up, then try again.",
                brand::CLI_BIN
            )))
        }
    }
    let record = SandboxRecord::load(&project.dir).map_err(internal)?;
    let Some(baseline) = record.and_then(|r| r.baseline) else {
        return Err(no_baseline());
    };
    if baseline.len() != 40 || !crate::gitref::is_commit_id(&baseline) {
        return Err(internal(
            "the record of what this sandbox started from is damaged",
        ));
    }
    // A project cloned from a remote started at a commit the host's own
    // mirror holds; any other project, at the commit the host made of its
    // snapshot.
    let store = crate::remote::mirror_of(service, project)
        .unwrap_or_else(|| project.dir.join(BASELINE_STORE));
    if !std::fs::symlink_metadata(&store).is_ok_and(|m| m.is_dir()) {
        return Err(no_baseline());
    }

    let staging = tempdir_in_state(service)?;
    let bundle = staging.path().join("from-sandbox.bundle");
    progress.step("bundling inside the sandbox", None);
    fetch_bundle(service, project, &baseline, &bundle, &*progress).map_err(not_reviewed)?;
    progress.step("reviewing on the host", None);
    let (quarantine, review) = review_bundle(
        &store,
        &baseline,
        &bundle,
        &staging.path().join("quarantine.git"),
    )
    .map_err(not_reviewed)?;
    let mut report = report(project, &baseline, &review);
    progress.step("reading the changed lines", None);
    // A keychain that cannot be read means values cannot be looked for. The
    // comparison is still worth having, without any of the file content.
    if let Ok(masker) = crate::mask::Masker::for_project(service, project) {
        fill_patches(&mut report, &quarantine, &review, &masker, &*progress);
    }
    service.record(
        audit::entry(service.deps().now_unix(), audit::Action::ChangesReviewed)
            .project(project.id.as_str())
            .detail(format!(
                "{} files changed, {} flags",
                report.files.len(),
                report.flags.len()
            )),
    );
    Ok(report)
}

/// Runs the guest script and writes the decoded bundle to `dest`, a new file
/// in the daemon's own staging folder.
fn fetch_bundle(
    service: &Service,
    project: &Project,
    baseline: &str,
    dest: &Path,
    progress: &dyn Progress,
) -> Result<(), ApiError> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dest)
        .map_err(|e| internal(format!("cannot make a file for the bundle: {}", e.kind())))?;
    let mut request = ExecRequest::new(["sh", "-c", GUEST_SCRIPT, "sh", baseline]);
    request.workdir = Some(GuestPath::new(WORK_DIR).map_err(|e| internal(e.to_string()))?);
    request.timeout = Some(BUNDLE_LIMIT);
    let mut sink = BundleSink::new(file, MAX_BUNDLE_BYTES, progress);
    let end = service
        .deps()
        .engine()
        .exec_stream(&project.sandbox, &request, &mut sink)
        .map_err(|e| engine_error(e.to_string()))?;
    if let Some(problem) = sink.problem.take() {
        return Err(refused(problem));
    }
    let said = text::escape_short(String::from_utf8_lossy(&sink.stderr).trim(), 1500);
    match end {
        StreamEnd::Exited(0) => {}
        StreamEnd::Exited(code) => {
            return Err(engine_error(format!(
                "the sandbox could not make a bundle of its changes (exit code {code}): {said}"
            )))
        }
        StreamEnd::Stopped => return Err(engine_error("making the bundle was stopped")),
    }
    sink.finish().map_err(refused)?;
    Ok(())
}

/// Takes base64 from the guest and writes what it decodes to. It stops the
/// command at the first byte over the limit or the first byte that is not
/// base64, and keeps a little of what the guest wrote to stderr.
struct BundleSink<'a> {
    file: File,
    decoder: Base64,
    written: u64,
    limit: u64,
    problem: Option<String>,
    stderr: Vec<u8>,
    progress: &'a dyn Progress,
}

impl<'a> BundleSink<'a> {
    fn new(file: File, limit: u64, progress: &'a dyn Progress) -> Self {
        Self {
            file,
            decoder: Base64::default(),
            written: 0,
            limit,
            problem: None,
            stderr: Vec::new(),
            progress,
        }
    }

    fn finish(&mut self) -> Result<(), String> {
        if !self.decoder.complete() {
            return Err("the sandbox's bundle ended part way through".to_owned());
        }
        if self.written == 0 {
            return Err("the sandbox sent no bundle".to_owned());
        }
        self.file
            .sync_all()
            .map_err(|e| format!("cannot write the bundle: {}", e.kind()))
    }
}

impl OutputSink for BundleSink<'_> {
    fn output(&mut self, stream: engine::OutputStream, bytes: &[u8]) -> bool {
        if stream == OutputStream::Stderr {
            let room = MAX_STDERR.saturating_sub(self.stderr.len());
            self.stderr
                .extend_from_slice(&bytes[..bytes.len().min(room)]);
            return true;
        }
        let mut decoded = Vec::with_capacity(bytes.len() / 4 * 3 + 3);
        if self.decoder.feed(bytes, &mut decoded).is_err() {
            self.problem = Some(
                "the sandbox's bundle was not plain base64, so it was refused. A secret's value appearing in it and being masked on the way out also causes this.".to_owned(),
            );
            return false;
        }
        self.written = self.written.saturating_add(decoded.len() as u64);
        if self.written > self.limit {
            self.problem = Some(format!(
                "the sandbox's bundle was over the limit of {} MB, so it was refused",
                self.limit / (1024 * 1024)
            ));
            return false;
        }
        if let Err(e) = self.file.write_all(&decoded) {
            self.problem = Some(format!("cannot write the bundle: {}", e.kind()));
            return false;
        }
        true
    }

    fn cancelled(&self) -> bool {
        self.progress.cancelled()
    }
}

const PAD: u8 = 64;

/// A strict base64 decoder that takes its input in pieces. Line breaks are
/// skipped; anything else outside the alphabet, or after the padding, is an
/// error.
#[derive(Default)]
struct Base64 {
    quad: [u8; 4],
    filled: usize,
    ended: bool,
}

impl Base64 {
    fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<(), ()> {
        for &byte in input {
            let value = match byte {
                b'\n' | b'\r' => continue,
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => PAD,
                _ => return Err(()),
            };
            if self.ended {
                return Err(());
            }
            self.quad[self.filled] = value;
            self.filled += 1;
            if self.filled < 4 {
                continue;
            }
            self.filled = 0;
            let [a, b, c, d] = self.quad;
            if a == PAD || b == PAD || (c == PAD && d != PAD) {
                return Err(());
            }
            out.push(a << 2 | b >> 4);
            if c == PAD {
                self.ended = true;
                continue;
            }
            out.push((b & 0x0f) << 4 | c >> 2);
            if d == PAD {
                self.ended = true;
                continue;
            }
            out.push((c & 0x03) << 6 | d);
        }
        Ok(())
    }

    fn complete(&self) -> bool {
        self.filled == 0
    }
}

/// Imports the bundle into a new quarantine next to it and reviews it against
/// the host's own copy of the baseline.
fn review_bundle(
    store: &Path,
    baseline: &str,
    bundle: &Path,
    quarantine_dir: &Path,
) -> Result<(Quarantine, Review), ApiError> {
    let host_side = |e: gitbroker::Error| match e {
        gitbroker::Error::GitUnavailable(_) | gitbroker::Error::GitTooOld { .. } => internal(
            format!("git on this machine cannot be used for a review: {e}"),
        ),
        other => internal(format!("cannot prepare the review: {other}")),
    };
    let mut quarantine = Quarantine::create(quarantine_dir).map_err(host_side)?;
    quarantine.import_base(store, baseline).map_err(host_side)?;
    let imported = quarantine
        .import_bundle(bundle, MAX_BUNDLE_BYTES)
        .map_err(|e| match e {
            gitbroker::Error::GitUnavailable(_) | gitbroker::Error::GitTooOld { .. } => {
                host_side(e)
            }
            other => refused(format!("the sandbox's bundle was refused: {other}")),
        })?;
    let tip = match imported.refs.as_slice() {
        [only] if only.bundle_ref == TIP_REF => only.object_id.clone(),
        _ => return Err(refused(
            "the sandbox's bundle did not hold exactly the one branch asked for, so it was refused",
        )),
    };
    let review = quarantine
        .review(Some(baseline), &tip)
        .map_err(|e| refused(format!("the sandbox's changes could not be reviewed: {e}")))?;
    Ok((quarantine, review))
}

/// Puts the changed lines into a report, as many as one message can carry.
///
/// Files with a flag go first, so a sandbox cannot push its planted hook out of
/// view by also changing a lot of harmless text. Whatever does not fit stays
/// [`FilePatch::NotIncluded`], which every client calls unread.
fn fill_patches(
    report: &mut DiffReport,
    quarantine: &Quarantine,
    review: &Review,
    masker: &crate::mask::Masker,
    progress: &dyn Progress,
) {
    let limits = PatchLimits::default();
    let started = Instant::now();
    let holds_secret = |blob: &[u8]| {
        let text = String::from_utf8_lossy(blob);
        masker.mask(&text) != text
    };
    let Ok(base) = serde_json::to_vec(&*report) else {
        return;
    };
    let mut room = MAX_FRAME_BYTES.saturating_sub(base.len() + FRAME_MARGIN);

    let flagged: HashSet<String> = report.flags.iter().filter_map(|f| f.path.clone()).collect();
    let mut order: Vec<usize> = (0..report.files.len()).collect();
    order.sort_by_key(|at| !flagged.contains(&report.files[*at].path));

    for at in order.into_iter().take(MAX_PATCHED_FILES) {
        if room == 0 || progress.cancelled() || started.elapsed() > PATCH_TIME_LIMIT {
            break;
        }
        let Some(change) = review.changes().get(at) else {
            continue;
        };
        let patch = match review.patch(quarantine, change, &limits, &holds_secret) {
            Ok(patch) => file_patch(patch),
            Err(_) => FilePatch::Unreadable,
        };
        let Ok(encoded) = serde_json::to_vec(&patch) else {
            continue;
        };
        if encoded.len() <= room {
            room -= encoded.len();
            report.files[at].patch = patch;
        }
    }
}

/// Escapes one file's lines and cuts them to its share of the message.
fn file_patch(patch: Patch) -> FilePatch {
    let (hunks, added, removed, mut truncated) = match patch {
        Patch::Text { hunks, added, removed, truncated } => (hunks, added, removed, truncated),
        Patch::Withheld => return FilePatch::Withheld,
        Patch::Binary => return FilePatch::Binary,
        Patch::TooLarge => return FilePatch::TooLarge,
        Patch::Submodule => return FilePatch::Submodule,
        Patch::Unreadable => return FilePatch::Unreadable,
    };
    let mut bytes = 0;
    let mut out = Vec::new();
    'hunks: for hunk in hunks {
        let mut lines = Vec::with_capacity(hunk.lines.len());
        for line in hunk.lines {
            let mut text = text::escape_line(&String::from_utf8_lossy(&line.text), MAX_LINE_CHARS);
            if line.no_newline {
                text.push_str(NO_NEWLINE);
            }
            // Twice the text: JSON can double every character of it.
            bytes += text.len() * 2 + LINE_OVERHEAD;
            if bytes > MAX_FILE_PATCH_BYTES {
                truncated = true;
                if !lines.is_empty() {
                    out.push(DiffHunk {
                        old_start: hunk.old_start,
                        new_start: hunk.new_start,
                        lines,
                    });
                }
                break 'hunks;
            }
            let kind = match line.kind {
                LineKind::Context => DiffLineKind::Context,
                LineKind::Added => DiffLineKind::Added,
                LineKind::Removed => DiffLineKind::Removed,
            };
            lines.push(DiffLine { kind, text });
        }
        out.push(DiffHunk {
            old_start: hunk.old_start,
            new_start: hunk.new_start,
            lines,
        });
    }
    FilePatch::Text {
        hunks: out,
        added,
        removed,
        truncated,
    }
}

fn report(project: &Project, baseline: &str, review: &Review) -> DiffReport {
    let mut flags: Vec<(u8, DiffFlag)> = review
        .flags()
        .iter()
        .take(MAX_FLAGS)
        .map(|flag| (rank(flag.kind), describe(flag)))
        .collect();
    // Stable, so flags of one rank keep the review's order.
    flags.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.path.cmp(&b.1.path)));
    DiffReport {
        project_id: project.id.as_str().to_owned(),
        baseline: baseline.to_owned(),
        flags: flags.into_iter().map(|(_, flag)| flag).collect(),
        files: review
            .changes()
            .iter()
            .take(MAX_FILES)
            .map(|change| DiffFile {
                path: shown_path(&change.display_path),
                change: match change.status {
                    ChangeStatus::Added => FileChange::Added,
                    ChangeStatus::Modified => FileChange::Modified,
                    ChangeStatus::Deleted => FileChange::Deleted,
                    ChangeStatus::TypeChanged => FileChange::TypeChanged,
                },
                patch: FilePatch::NotIncluded,
            })
            .collect(),
        commits: review.commits_scanned() as u64,
        complete: review.complete(),
    }
}

/// The broker quotes every path. The quotes are dropped where nothing inside
/// needs them, so an ordinary path reads as itself, and kept otherwise.
fn shown_path(display: &str) -> String {
    let inner = display
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .filter(|inner| !inner.is_empty() && !inner.contains(['"', '\\', ' ']));
    text::escape_short(inner.unwrap_or(display), MAX_PATH_CHARS)
}

/// Lower comes first. What would run on the person's own machine leads.
fn rank(kind: FlagKind) -> u8 {
    match kind {
        FlagKind::AgentHook => 0,
        FlagKind::ClaudeConfig
        | FlagKind::CursorConfig
        | FlagKind::VscodeConfig
        | FlagKind::GitHooks
        | FlagKind::InstallScript => 1,
        FlagKind::LikelySecret(_)
        | FlagKind::AgentInstructions
        | FlagKind::GithubWorkflow
        | FlagKind::SuspiciousPath
        | FlagKind::BaseNotAncestor
        | FlagKind::ScanIncomplete => 2,
        FlagKind::Gitmodules
        | FlagKind::Gitattributes
        | FlagKind::Gitlink
        | FlagKind::Symlink
        | FlagKind::PackageScripts
        | FlagKind::Makefile
        | FlagKind::EnvFile
        | FlagKind::BlobNotScanned => 3,
        FlagKind::Dockerfile
        | FlagKind::ComposeFile
        | FlagKind::Lockfile
        | FlagKind::NewExecutable
        | FlagKind::Binary => 4,
    }
}

fn kind_name(kind: FlagKind) -> &'static str {
    match kind {
        FlagKind::GithubWorkflow => "github_workflow",
        FlagKind::VscodeConfig => "vscode_config",
        FlagKind::ClaudeConfig => "claude_config",
        FlagKind::CursorConfig => "cursor_config",
        FlagKind::AgentHook => "agent_hook",
        FlagKind::AgentInstructions => "agent_instructions",
        FlagKind::Gitmodules => "gitmodules",
        FlagKind::Gitattributes => "gitattributes",
        FlagKind::GitHooks => "git_hooks",
        FlagKind::Makefile => "makefile",
        FlagKind::Dockerfile => "dockerfile",
        FlagKind::ComposeFile => "compose_file",
        FlagKind::Lockfile => "lockfile",
        FlagKind::EnvFile => "env_file",
        FlagKind::PackageScripts => "package_scripts",
        FlagKind::InstallScript => "install_script",
        FlagKind::Symlink => "symlink",
        FlagKind::Gitlink => "gitlink",
        FlagKind::NewExecutable => "new_executable",
        FlagKind::Binary => "binary",
        FlagKind::LikelySecret(_) => "likely_secret",
        FlagKind::SuspiciousPath => "suspicious_path",
        FlagKind::BlobNotScanned => "blob_not_scanned",
        FlagKind::ScanIncomplete => "scan_incomplete",
        FlagKind::BaseNotAncestor => "base_not_ancestor",
    }
}

fn describe(flag: &Flag) -> DiffFlag {
    let detail = flag
        .detail
        .as_deref()
        .map(|d| text::escape_short(d, 200))
        .unwrap_or_default();
    let with_detail = |base: &str| {
        if detail.is_empty() {
            base.to_owned()
        } else {
            format!("{base} ({detail})")
        }
    };
    let message = match flag.kind {
        FlagKind::AgentHook => {
            "adds a hook for an AI coding tool, which would run on your machine".to_owned()
        }
        FlagKind::ClaudeConfig => {
            "changes Claude Code settings, which can make it run commands on your machine".to_owned()
        }
        FlagKind::CursorConfig => {
            "changes Cursor settings or rules, which steer an AI coding tool on your machine".to_owned()
        }
        FlagKind::VscodeConfig => {
            "changes editor settings or tasks, which can run commands when the folder is opened".to_owned()
        }
        FlagKind::AgentInstructions => {
            "changes instructions that an AI coding agent reads and follows".to_owned()
        }
        FlagKind::GithubWorkflow => {
            "changes a CI workflow, which runs with the repository's secrets".to_owned()
        }
        FlagKind::GitHooks => "adds or changes Git hooks, which run on your machine".to_owned(),
        FlagKind::Gitmodules => {
            "changes submodules, which make Git fetch other repositories".to_owned()
        }
        FlagKind::Gitattributes => {
            "changes .gitattributes, which can change how files are shown and filtered".to_owned()
        }
        FlagKind::Makefile => "changes a Makefile, which runs commands on make".to_owned(),
        FlagKind::Dockerfile => "changes a Dockerfile, which runs commands at build".to_owned(),
        FlagKind::ComposeFile => {
            "changes a Compose file, which decides what runs and what it can reach".to_owned()
        }
        FlagKind::Lockfile => {
            "changes a lockfile, which decides which dependency code is installed".to_owned()
        }
        FlagKind::EnvFile => "adds or changes an environment file, which often holds secrets".to_owned(),
        FlagKind::PackageScripts => with_detail("changes package.json scripts"),
        FlagKind::InstallScript => {
            with_detail("changes an install script, which runs on npm install")
        }
        FlagKind::Symlink => "is a symbolic link, which can point outside the project".to_owned(),
        FlagKind::Gitlink => "is a submodule pointer to another repository".to_owned(),
        FlagKind::NewExecutable => "is newly executable".to_owned(),
        FlagKind::Binary => "is a binary file, which cannot be read as text".to_owned(),
        FlagKind::LikelySecret(rule) => {
            let base = format!("looks like it holds a secret ({})", secret_rule(rule));
            if detail.is_empty() {
                base
            } else {
                format!("{base}, {detail}")
            }
        }
        FlagKind::SuspiciousPath => {
            "has a name with hidden, control or invalid characters".to_owned()
        }
        FlagKind::BlobNotScanned => {
            "was too large or unreadable, so its content was not checked".to_owned()
        }
        FlagKind::ScanIncomplete => {
            with_detail("the review stopped at a limit, so this list may not be complete")
        }
        FlagKind::BaseNotAncestor => {
            "the sandbox's history does not start from the snapshot, so this may not show everything".to_owned()
        }
    };
    DiffFlag {
        kind: kind_name(flag.kind).to_owned(),
        path: flag.path.as_deref().map(shown_path),
        line: flag.line,
        message,
        earlier_commit: flag.commit.is_some(),
    }
}

fn secret_rule(rule: gitbroker::SecretRule) -> &'static str {
    use gitbroker::SecretRule as R;
    match rule {
        R::AwsAccessKey => "an AWS access key",
        R::AwsSecretKey => "an AWS secret key",
        R::GithubToken => "a GitHub token",
        R::PrivateKey => "a private key",
        R::AnthropicKey => "an Anthropic API key",
        R::NpmToken => "an npm token",
        R::HighEntropy => "a random-looking value",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(pieces: &[&[u8]]) -> Result<Vec<u8>, ()> {
        let mut decoder = Base64::default();
        let mut out = Vec::new();
        for piece in pieces {
            decoder.feed(piece, &mut out)?;
        }
        if decoder.complete() {
            Ok(out)
        } else {
            Err(())
        }
    }

    #[test]
    fn base64_decodes_in_any_pieces() {
        assert_eq!(
            decode(&[b"aGVsbG8gd29y", b"bGQ=\n"]).unwrap(),
            b"hello world"
        );
        assert_eq!(decode(&[b"aG", b"Vsb", b"G8=", b"\n"]).unwrap(), b"hello");
        assert_eq!(decode(&[b"YQ==\r\n"]).unwrap(), b"a");
        assert_eq!(decode(&[b""]).unwrap(), b"");
        let all: Vec<u8> = (0..=255).collect();
        let encoded = b"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4\nOTo7PD0+P0BBQkNERUZHSElKS0xNTk9QUVJTVFVWV1hZWltcXV5fYGFiY2RlZmdoaWprbG1ub3Bx\ncnN0dXZ3eHl6e3x9fn+AgYKDhIWGh4iJiouMjY6PkJGSk5SVlpeYmZqbnJ2en6ChoqOkpaanqKmq\nq6ytrq+wsbKztLW2t7i5uru8vb6/wMHCw8TFxsfIycrLzM3Oz9DR0tPU1dbX2Nna29zd3t/g4eLj\n5OXm5+jp6uvs7e7v8PHy8/T19vf4+fr7/P3+/w==\n";
        assert_eq!(decode(&[encoded]).unwrap(), all);
    }

    #[test]
    fn base64_refuses_anything_else() {
        for bad in [
            &b"aGVs bG8="[..],
            b"aGVsbG8=aGVs",
            b"[REDACTED]",
            b"=aaa",
            b"aa=a",
            b"aGVsbG8",
            b"\x1b[31m",
        ] {
            assert!(
                decode(&[bad]).is_err(),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    /// The limit holds on the bytes as they arrive, so a guest that sends
    /// without end is stopped, and what it sent is never all held.
    #[test]
    fn a_bundle_over_the_limit_stops_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b");
        let file = std::fs::File::create(&path).unwrap();
        let mut sink = BundleSink::new(file, 30, &crate::Quiet);
        assert!(sink.output(OutputStream::Stdout, b"QUFBQUFBQUFBQUFBQUFBQUFBQUFB\n"));
        assert!(!sink.output(OutputStream::Stdout, b"QUFBQUFBQUFBQUFBQUFBQUFBQUFB\n"));
        assert!(sink.problem.as_deref().unwrap().contains("over the limit"));
        assert!(std::fs::metadata(&path).unwrap().len() <= 30);
    }

    #[test]
    fn stderr_from_the_guest_is_kept_short() {
        let dir = tempfile::tempdir().unwrap();
        let file = std::fs::File::create(dir.path().join("b")).unwrap();
        let mut sink = BundleSink::new(file, 30, &crate::Quiet);
        for _ in 0..100 {
            assert!(sink.output(OutputStream::Stderr, &[b'x'; 1000]));
        }
        assert_eq!(sink.stderr.len(), MAX_STDERR);
    }

    #[test]
    fn plain_paths_lose_their_quotes_and_odd_ones_keep_them() {
        assert_eq!(
            shown_path("\".claude/settings.json\""),
            ".claude/settings.json"
        );
        assert_eq!(shown_path("\"my file.txt\""), "\"my file.txt\"");
        assert_eq!(shown_path("\"a\\nb\""), "\"a\\nb\"");
        assert_eq!(shown_path("\"\""), "\"\"");
        assert!(!shown_path("\"\u{1b}[31m\"").contains('\u{1b}'));
    }

    /// The order a person reads them in: what runs on their machine first.
    #[test]
    fn a_planted_hook_outranks_everything() {
        let kinds = [
            FlagKind::Lockfile,
            FlagKind::ClaudeConfig,
            FlagKind::AgentHook,
            FlagKind::LikelySecret(gitbroker::SecretRule::NpmToken),
        ];
        let first = kinds.iter().min_by_key(|k| rank(**k)).unwrap();
        assert_eq!(*first, FlagKind::AgentHook);
    }

    /// Plain git for setting up a stand-in for the guest's `/work`, never with
    /// the user's own config.
    fn plain_git(dir: &Path, args: &[&str]) -> std::process::Output {
        std::process::Command::new("git")
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
    }

    /// Runs a guest script for real on this machine, with `work` standing in
    /// for `/work`.
    fn run_guest(script: &str, work: &Path, base: &str) -> std::process::Output {
        let script = script.replace("/work", work.to_str().unwrap());
        std::process::Command::new("sh")
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", work)
            .args(["-c", &script, "sh", base])
            .current_dir(work)
            .output()
            .unwrap()
    }

    /// Seen on a real VM: a snapshot made on a Mac carried `core.ignorecase=true`
    /// into the guest, and a second copy of `.claude/settings.json` that differed
    /// only in case stopped the bundle, so nothing was reviewed at all. This needs
    /// a folder that tells case apart, as the guest's disk does. A Mac's usual
    /// disk does not, so there it says so and checks nothing; run it with
    /// `TMPDIR` on a case-sensitive volume to see it on a Mac.
    #[test]
    fn paths_that_differ_only_in_case_are_bundled_whatever_the_repository_says() {
        let temp = tempfile::tempdir().unwrap();
        let work = temp.path().canonicalize().unwrap().join("work");
        std::fs::create_dir(&work).unwrap();
        std::fs::write(work.join("case"), "").unwrap();
        if work.join("CASE").exists() {
            eprintln!("skipped: {} does not tell case apart", work.display());
            return;
        }
        std::fs::remove_file(work.join("case")).unwrap();
        let ok = |out: std::process::Output| {
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8(out.stdout).unwrap()
        };
        ok(plain_git(&work, &["init", "-q", "-b", "main"]));
        ok(plain_git(&work, &["config", "core.ignorecase", "true"]));
        std::fs::write(work.join("app.js"), "1\n").unwrap();
        ok(plain_git(&work, &["add", "--all"]));
        ok(plain_git(&work, &["commit", "-q", "-m", "base"]));
        let base = ok(plain_git(&work, &["rev-parse", "HEAD"])).trim().to_owned();
        for (dir, file) in [(".claude", "settings.json"), (".CLAUDE", "Settings.json")] {
            std::fs::create_dir(work.join(dir)).unwrap();
            std::fs::write(work.join(dir).join(file), "{\"hooks\":{}}\n").unwrap();
        }

        // The script as it was fails just as it did on the real VM.
        let before = GUEST_SCRIPT.replace(
            " \\\n      -c core.ignorecase=false -c core.precomposeunicode=false",
            "",
        );
        assert_ne!(before, GUEST_SCRIPT);
        let failed = run_guest(&before, &work, &base);
        assert!(!failed.status.success());
        assert!(
            String::from_utf8_lossy(&failed.stderr).contains("will not add file alias"),
            "{}",
            String::from_utf8_lossy(&failed.stderr)
        );

        let sent = run_guest(GUEST_SCRIPT, &work, &base);
        assert!(sent.status.success(), "{}", String::from_utf8_lossy(&sent.stderr));
        let mut bundle = Vec::new();
        let mut decoder = Base64::default();
        decoder.feed(&sent.stdout, &mut bundle).unwrap();
        assert!(decoder.complete());
        let file = temp.path().join("out.bundle");
        std::fs::write(&file, bundle).unwrap();
        let into = temp.path().canonicalize().unwrap().join("check");
        std::fs::create_dir(&into).unwrap();
        ok(plain_git(&into, &["init", "-q"]));
        ok(plain_git(&into, &["fetch", "-q", work.to_str().unwrap(), &base]));
        ok(plain_git(&into, &["fetch", "-q", file.to_str().unwrap(), &format!("{TIP_REF}:{TIP_REF}")]));
        let files = ok(plain_git(&into, &["ls-tree", "-r", "--name-only", TIP_REF]));
        assert_eq!(
            files.lines().collect::<Vec<_>>(),
            [".CLAUDE/Settings.json", ".claude/settings.json", "app.js"]
        );
    }

    #[test]
    fn the_guest_script_names_the_branch_the_host_accepts() {
        let branch = TIP_REF.strip_prefix("refs/heads/").unwrap();
        assert!(GUEST_SCRIPT.contains(&format!("refs/heads/{branch} \"$tip\"")));
        assert_eq!(GUEST_SCRIPT.matches(TIP_REF).count(), 3);
    }
}
