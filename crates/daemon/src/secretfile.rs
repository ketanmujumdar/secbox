//! Using an env file for a project's secrets: pick the file, see its names,
//! agree, and its values go to the keychain and on to the chosen services.
//!
//! **Names only come back.** No reply holds a value, so a client gains no way
//! to read a file's secrets by asking for them here.
//!
//! **A sandbox's file is hostile data.** It is read through one fixed command,
//! with a size limit enforced as the bytes arrive, parsed as data and never
//! run. A compromised sandbox can make up the file or change it at any moment.
//! That is why storing it takes the names the person saw: one added after they
//! looked is left out. It can still choose the values, which then go back only
//! into that sandbox, and can replace a stored value, which the preview names
//! first.
//!
//! **A file on this machine** goes through [`secrets::parse_env_file`], which
//! refuses links and devices, and must be absolute, so the daemon's own working
//! folder never decides which file is meant.
//!
//! This is one copy, made when asked, and not a link: a later change to the
//! file changes nothing until it is used again. A file that was re-read on each
//! start would let a compromised sandbox rewrite the keychain unseen.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use engine::{ExecRequest, GuestPath, OutputSink, OutputStream, StreamEnd};
use proto::{
    text, ApiError, SandboxState, SecretFileEffect, SecretFileName, SecretFilePreview,
    SecretFileSource, SecretFilesReport, SkippedSecret, UseSecretFileArgs, UseSecretFileReport,
    MAX_SECRET_BYTES,
};
use secrets::{
    is_env_file_name, is_template_file, parse_env_bytes, parse_env_file, Assignment, AssignmentSet,
    AuditAction, Delivery, EnvEntry, MAX_ENV_FILE_BYTES,
};
use zeroize::Zeroizing;

use crate::run::{status_of, WORK_DIR};
use crate::secretset::checked_services;
use crate::{audit, bad, engine_error, internal, plan, refused, wrong_state, Project, Service};

/// How long a sandbox may take to answer.
const GUEST_LIMIT: Duration = Duration::from_secs(15);
/// The most files listed from either place, and the longest path used.
const MAX_LISTED: usize = 100;
const MAX_PATH_CHARS: usize = 400;
/// How much of the listing is read from a sandbox.
const MAX_LISTING_BYTES: usize = 128 * 1024;
const FIND_SCRIPT: &str = "find . -maxdepth 5 \\( -name node_modules -o -name .git \\) -prune -o \
     -type f \\( -name .env -o -name '.env.*' -o -name '*.env' \\) -print";
/// Exit 3 is what this script says for anything but a plain file. A link is
/// refused here as it is on the host.
const READ_SCRIPT: &str = "[ -f \"$1\" ] && [ ! -L \"$1\" ] || exit 3; exec head -c \"$2\" -- \"$1\"";

/// The env files in a running sandbox, and at the top of the project's folder.
pub fn find(service: &Service, project: &Project) -> Result<SecretFilesReport, ApiError> {
    let mut report = SecretFilesReport {
        sandbox: Vec::new(),
        host: host_files(&project.root),
        note: None,
    };
    if !running(service, project) {
        report.note = Some("The sandbox is not running, so its files can't be read.".to_owned());
        return Ok(report);
    }
    match read_guest(service, project, &["sh", "-c", FIND_SCRIPT], MAX_LISTING_BYTES) {
        Ok(bytes) => report.sandbox = guest_files(&bytes),
        Err(error) => report.note = Some(error.message),
    }
    Ok(report)
}

/// The file's names, and what using it would do to each.
pub fn preview(
    service: &Service,
    project: &Project,
    source: &SecretFileSource,
) -> Result<SecretFilePreview, ApiError> {
    let entries = read_entries(service, project, source)?;
    let names_only = names_only(source);
    let meta = hoststate::read_meta(&project.dir).map_err(internal)?;
    let names = entries
        .iter()
        .map(|entry| {
            let stored = meta.as_ref().is_some_and(|m| m.has(&entry.name));
            let empty = names_only || entry.value.is_empty();
            let delivery = meta.as_ref().and_then(|m| delivery_of(m, &entry.name));
            SecretFileName {
                name: text::escape(entry.name.as_str()),
                effect: match (empty, stored) {
                    (true, true) => SecretFileEffect::Keep,
                    (true, false) => SecretFileEffect::Empty,
                    (false, _) if refusal(entry.value.expose(), delivery.as_ref()).is_some() => {
                        SecretFileEffect::Skip
                    }
                    (false, true) => SecretFileEffect::Replace,
                    (false, false) => SecretFileEffect::Store,
                },
            }
        })
        .collect();
    let (services, services_note) = match plan::service_names(project) {
        Ok(names) => (names.iter().map(|n| text::escape(n)).collect(), None),
        Err(error) => (Vec::new(), Some(error.message)),
    };
    Ok(SecretFilePreview {
        source: source.clone(),
        names,
        names_only,
        services,
        services_note,
    })
}

/// Stores the agreed names' values. Each one is recorded straight after the
/// keychain takes it, so a failure part way leaves nothing unrecorded.
pub fn apply(
    service: &Service,
    project: &Project,
    args: &UseSecretFileArgs,
) -> Result<UseSecretFileReport, ApiError> {
    let services = checked_services(project, &args.services)?;
    let entries = read_entries(service, project, &args.source)?;
    let names_only = names_only(&args.source);
    let agreed: HashSet<&str> = args.names.iter().map(String::as_str).collect();

    let mut meta = hoststate::read_meta(&project.dir)
        .map_err(internal)?
        .unwrap_or_else(|| hoststate::ProjectMeta::new(&project.root));
    let store = service.deps().store(&project.id);
    let mut report = UseSecretFileReport {
        stored: Vec::new(),
        replaced: Vec::new(),
        empty: Vec::new(),
        skipped: Vec::new(),
        services: services.iter().map(|s| s.as_str().to_owned()).collect(),
        applies_on_next_start: running(service, project),
    };

    for entry in entries.iter().filter(|e| agreed.contains(e.name.as_str())) {
        let name = &entry.name;
        if names_only || entry.value.is_empty() {
            if meta.add_pending(name) {
                hoststate::write_meta(&project.dir, &meta).map_err(internal)?;
                report.empty.push(name.as_str().to_owned());
            }
            continue;
        }
        let delivery = delivery_of(&meta, name);
        if let Some(reason) = refusal(entry.value.expose(), delivery.as_ref()) {
            report.skipped.push(SkippedSecret {
                name: name.as_str().to_owned(),
                reason: reason.to_owned(),
            });
            continue;
        }
        let mut all: Vec<Assignment> = meta
            .assignments
            .iter()
            .filter(|a| a.name() != name)
            .cloned()
            .collect();
        if !services.is_empty() {
            all.push(
                Assignment::new(
                    name.clone(),
                    delivery.unwrap_or(Delivery::Injected),
                    services.clone(),
                )
                .map_err(|e| bad(text::escape(&e.to_string())))?,
            );
        }
        let assignments = AssignmentSet::new(all).map_err(|e| bad(text::escape(&e.to_string())))?;
        let replaced = meta.has(name);
        store.put(name, &entry.value).map_err(|e| {
            internal(format!("the keychain refused: {}", text::escape(&e.to_string())))
        })?;
        meta.record(name, hoststate::SecretSource::Import);
        meta.assignments = assignments;
        hoststate::write_meta(&project.dir, &meta).map_err(internal)?;
        hoststate::audit(&project.dir, AuditAction::Imported, name).map_err(internal)?;
        hoststate::audit(&project.dir, AuditAction::AssignmentChanged, name).map_err(internal)?;
        service.record(
            audit::entry(service.deps().now_unix(), audit::Action::SecretStored)
                .project(project.id.as_str())
                .subject(name.as_str())
                .detail(match &args.source {
                    SecretFileSource::Sandbox(_) => "stored from a file in the sandbox",
                    SecretFileSource::Host(_) => "stored from a file on this machine",
                }),
        );
        if replaced {
            report.replaced.push(name.as_str().to_owned());
        } else {
            report.stored.push(name.as_str().to_owned());
        }
    }
    Ok(report)
}

fn delivery_of(meta: &hoststate::ProjectMeta, name: &secrets::SecretName) -> Option<Delivery> {
    meta.assignments
        .iter()
        .find(|a| a.name() == name)
        .map(|a| a.delivery().clone())
}

/// Why a value can't be stored. A line break is refused here because a stored
/// one going into an environment would stop every later start of the project.
fn refusal(value: &str, delivery: Option<&Delivery>) -> Option<&'static str> {
    if value.len() > MAX_SECRET_BYTES {
        return Some("the value is too long");
    }
    let as_file = matches!(delivery, Some(Delivery::InjectedFile));
    if !as_file && (value.contains('\n') || value.contains('\r')) {
        return Some("the value has a line break, which can't go in an environment");
    }
    None
}

fn names_only(source: &SecretFileSource) -> bool {
    let (SecretFileSource::Sandbox(path) | SecretFileSource::Host(path)) = source;
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_template_file)
}

fn read_entries(
    service: &Service,
    project: &Project,
    source: &SecretFileSource,
) -> Result<Vec<EnvEntry>, ApiError> {
    // Parse errors are fixed text and a line number, never file content.
    let unreadable = |e: secrets::EnvError| bad(format!("that file can't be used: {e}"));
    match source {
        SecretFileSource::Host(path) => {
            if !Path::new(path).is_absolute() {
                return Err(bad("give the file's full path"));
            }
            parse_env_file(Path::new(path)).map_err(unreadable)
        }
        SecretFileSource::Sandbox(path) => {
            let path = checked_guest_path(path)?;
            if !running(service, project) {
                return Err(wrong_state("the sandbox is not running, so its files can't be read"));
            }
            let limit = (MAX_ENV_FILE_BYTES + 1).to_string();
            let bytes = read_guest(
                service,
                project,
                &["sh", "-c", READ_SCRIPT, "sh", path, &limit],
                MAX_ENV_FILE_BYTES + 1,
            )?;
            parse_env_bytes(&bytes).map_err(unreadable)
        }
    }
}

/// A path the sandbox's command may be given: inside `/work`, and not one that
/// `head` could take for an option.
fn checked_guest_path(path: &str) -> Result<&str, ApiError> {
    let safe = !path.is_empty()
        && path.chars().count() <= MAX_PATH_CHARS
        && !path.starts_with('/')
        && !path.starts_with('-')
        && !path.chars().any(char::is_control)
        && !path.split('/').any(|part| part == "..");
    if safe {
        Ok(path)
    } else {
        Err(bad("that is not a path inside the project"))
    }
}

/// Runs one fixed command in `/work` and keeps at most `limit` bytes. Going
/// past the limit stops the command, and a file that size is then refused by
/// the parser rather than used in part.
fn read_guest(
    service: &Service,
    project: &Project,
    command: &[&str],
    limit: usize,
) -> Result<Zeroizing<Vec<u8>>, ApiError> {
    let mut request = ExecRequest::new(command.iter().copied());
    request.workdir = Some(GuestPath::new(WORK_DIR).map_err(|e| internal(e.to_string()))?);
    request.timeout = Some(GUEST_LIMIT);
    let mut sink = Capture {
        bytes: Zeroizing::new(Vec::with_capacity(limit)),
        limit,
    };
    let end = service
        .deps()
        .engine()
        .exec_stream(&project.sandbox, &request, &mut sink)
        .map_err(|e| engine_error(e.to_string()))?;
    match end {
        StreamEnd::Exited(0) => Ok(sink.bytes),
        StreamEnd::Exited(3) => Err(refused("that is not a plain file in the sandbox")),
        StreamEnd::Exited(code) => Err(engine_error(format!(
            "the sandbox could not read it (exit code {code})"
        ))),
        // The limit stopped it, so the parser's size check refuses the file.
        StreamEnd::Stopped if sink.bytes.len() >= limit => Ok(sink.bytes),
        StreamEnd::Stopped => Err(engine_error("reading from the sandbox was stopped")),
    }
}

/// Keeps standard output in a buffer that is never reallocated, so no unwiped
/// copy of a value is left behind. Standard error is dropped: it could hold
/// anything, and nothing here shows it.
struct Capture {
    bytes: Zeroizing<Vec<u8>>,
    limit: usize,
}

impl OutputSink for Capture {
    fn output(&mut self, stream: OutputStream, bytes: &[u8]) -> bool {
        if stream != OutputStream::Stdout {
            return true;
        }
        let room = self.limit - self.bytes.len();
        self.bytes.extend_from_slice(&bytes[..bytes.len().min(room)]);
        self.bytes.len() < self.limit
    }
}

fn running(service: &Service, project: &Project) -> bool {
    status_of(service, project)
        .map(|state| matches!(state, SandboxState::Running))
        .unwrap_or(false)
}

/// Paths from the sandbox's listing, sorted. One that needs escaping is left
/// out, since the escaped text could not name the same file again.
fn guest_files(listing: &[u8]) -> Vec<String> {
    let text_of = String::from_utf8_lossy(listing);
    let mut files: Vec<String> = text_of
        .lines()
        .map(|line| line.strip_prefix("./").unwrap_or(line))
        .filter(|path| checked_guest_path(path).is_ok() && text::escape(path) == *path)
        .filter(|path| {
            Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(is_env_file_name)
        })
        .map(str::to_owned)
        .collect();
    files.sort();
    files.dedup();
    files.truncate(MAX_LISTED);
    files
}

/// Env files at the top of the project's folder, which are usually the ones
/// Git ignores. Links are left out, as they would be refused when read.
fn host_files(root: &Path) -> Vec<String> {
    let Ok(listing) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut files: Vec<String> = listing
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .filter(|entry| entry.file_name().to_str().is_some_and(is_env_file_name))
        .filter_map(|entry| entry.path().to_str().map(str::to_owned))
        .filter(|path| text::escape(path) == *path)
        .collect();
    files.sort();
    files.truncate(MAX_LISTED);
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_outside_the_project_or_like_an_option_is_refused() {
        for path in ["", "/etc/passwd", "../x", "a/../../x", "-n", "a\nb", "a\u{1b}b"] {
            assert!(checked_guest_path(path).is_err(), "{path:?}");
        }
        for path in [".env", "app/.env.local", ".env.simulation"] {
            assert!(checked_guest_path(path).is_ok(), "{path:?}");
        }
    }

    #[test]
    fn the_listing_keeps_only_env_files_that_can_be_named_again() {
        let got = guest_files(b"./b/.env\n./.env.simulation\n./prod.env\n./notes.txt\n./-x.env\n./\x1b[31m.env\n");
        assert_eq!(got, vec![".env.simulation", "b/.env", "prod.env"]);
    }

    #[test]
    fn a_value_that_would_stop_every_start_is_not_stored() {
        assert!(refusal("a\nb", None).is_some());
        assert!(refusal("a\nb", Some(&Delivery::Injected)).is_some());
        assert!(refusal("a\nb", Some(&Delivery::InjectedFile)).is_none());
        assert!(refusal("plain", None).is_none());
        assert!(refusal(&"x".repeat(MAX_SECRET_BYTES + 1), Some(&Delivery::InjectedFile)).is_some());
    }

    #[test]
    fn a_template_says_so_by_its_name_wherever_it_is() {
        assert!(names_only(&SecretFileSource::Sandbox("app/.env.example".to_owned())));
        assert!(names_only(&SecretFileSource::Host("/p/.env.sample".to_owned())));
        assert!(!names_only(&SecretFileSource::Sandbox(".env.simulation".to_owned())));
    }
}
