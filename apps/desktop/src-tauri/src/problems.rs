//! What was refused or failed recently, read from the daemon's audit log.
//!
//! The daemon writes one JSON entry a line, on the host, whichever client
//! asked: the command line, this window or an AI agent. This reads the end of
//! that file, as the list of projects is read from the state folder, and keeps
//! only the entries that say something was refused, not confirmed or failed.
//!
//! Every field was escaped and cut by the daemon when it was written. It is
//! escaped again here, because the window must not trust a file it did not
//! write, and the page puts it in as text.

use std::collections::HashMap;
use std::os::unix::fs::OpenOptionsExt;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use proto::text;
use serde::{Deserialize, Serialize};

use crate::ops::{Note, Weight};

/// The end of the log that is read. Entries are short, so this holds
/// thousands of them.
const READ_LIMIT: u64 = 2 * 1024 * 1024;
/// Rows sent to the window, newest first.
const MAX_ROWS: usize = 200;
const MAX_FIELD: usize = 300;

/// One line of the daemon's audit log. Only the fields read here.
#[derive(Debug, Deserialize)]
struct Entry {
    at_unix: u64,
    action: String,
    project: Option<String>,
    subject: Option<String>,
    detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProblemRow {
    /// "2026-11-06 14:06:09", in UTC.
    pub time: String,
    /// The same moment in seconds since 1970, for the window to count recent ones.
    pub at_unix: u64,
    /// The project's folder name, or `None` when the request had no project,
    /// such as a fetch that never became one.
    pub project: Option<String>,
    /// The path the window opens the project by, if this machine knows it.
    pub project_path: Option<String>,
    /// What was asked for: "Push", "Start", "Fetch git@host:org/repo.git".
    pub what: String,
    /// Why it was refused or failed, in the daemon's words.
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProblemsView {
    pub rows: Vec<ProblemRow>,
    pub notes: Vec<Note>,
}

/// The problems for one project, given by its path, or for every project and
/// none with `None`.
pub fn problems(paths: &hoststate::StatePaths, project: Option<&str>) -> ProblemsView {
    let known: HashMap<String, (String, String)> = hoststate::known_projects(paths)
        .into_iter()
        .map(|(id, record)| {
            let name = Path::new(&record.path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| id.as_str().to_owned());
            (id.as_str().to_owned(), (name, record.path))
        })
        .collect();
    let wanted: Option<String> = match project {
        Some(path) => match known.iter().find(|(_, (_, known_path))| known_path == path) {
            Some((id, _)) => Some(id.clone()),
            None => {
                return ProblemsView {
                    rows: Vec::new(),
                    notes: vec![Note { weight: Weight::Warn, text: "This machine has no record of that project.".to_owned() }],
                }
            }
        },
        None => None,
    };

    let mut notes = Vec::new();
    let (text_read, cut) = match read_end(&paths.daemon_audit()) {
        Ok(read) => read,
        Err(reason) => {
            notes.push(Note { weight: Weight::Alert, text: format!("The audit log could not be read: {reason}.") });
            (String::new(), false)
        }
    };
    let mut unreadable = 0usize;
    let mut rows = Vec::new();
    for line in text_read.lines().rev() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Entry>(line) else {
            unreadable += 1;
            continue;
        };
        if let Some(id) = &wanted {
            if entry.project.as_deref() != Some(id.as_str()) {
                continue;
            }
        }
        let Some((what, why)) = describe(&entry) else { continue };
        let (name, path) = match entry.project.as_deref().and_then(|id| known.get(id)) {
            Some((name, path)) => (Some(name.clone()), Some(path.clone())),
            None => (entry.project.clone(), None),
        };
        rows.push(ProblemRow {
            time: utc_time(entry.at_unix),
            at_unix: entry.at_unix,
            project: name.map(|n| text::escape_short(&n, MAX_FIELD)),
            project_path: path.map(|p| text::escape(&p)),
            what: text::escape_short(&what, MAX_FIELD),
            why: text::escape_short(&why, MAX_FIELD),
        });
        if rows.len() >= MAX_ROWS {
            break;
        }
    }
    if rows.is_empty() && notes.is_empty() {
        notes.push(Note { weight: Weight::Plain, text: "Nothing has been refused recently.".to_owned() });
    }
    if cut || rows.len() >= MAX_ROWS {
        notes.push(Note { weight: Weight::Plain, text: "Only the most recent part of the audit log is shown.".to_owned() });
    }
    if unreadable > 0 {
        notes.push(Note {
            weight: Weight::Warn,
            text: format!("{unreadable} lines of the audit log could not be read, so they are not listed."),
        });
    }
    ProblemsView { rows, notes }
}

/// What was asked and why it did not happen, for the entries that are a
/// refusal or a failure. Everything else in the log is left out.
fn describe(entry: &Entry) -> Option<(String, String)> {
    let subject = entry.subject.as_deref().unwrap_or("");
    let detail = entry.detail.clone().unwrap_or_else(|| "no reason was recorded".to_owned());
    let what = match entry.action.as_str() {
        "git_push_refused" => "Push".to_owned(),
        // A push that was not confirmed is also recorded as a refused push,
        // with the same reason, so it is listed once.
        "person_declined" if subject == "push" => return None,
        "person_declined" => {
            let act = match subject {
                "start" => "Start".to_owned(),
                "allow host" => "Allow a hostname".to_owned(),
                other => other.to_owned(),
            };
            return Some((act, format!("Not confirmed: {detail}")));
        }
        "remote_fetch_failed" => format!("Fetch {subject}"),
        "sandbox_clone_failed" => format!("Clone {subject} into the sandbox"),
        // A port the person left unticked was their choice, not a refusal.
        "port_refused" if detail == "not approved" => return None,
        "port_refused" => format!("Forward port {subject}"),
        "secret_refused" => format!("Deliver secret {subject}"),
        "request_refused" if entry.project.is_some() => "Start".to_owned(),
        "request_refused" => "Connect to the daemon".to_owned(),
        "baseline_missing" => "Keep a copy to compare changes with".to_owned(),
        _ => return None,
    };
    Some((what, detail))
}

/// Reads at most the last [`READ_LIMIT`] bytes of a regular file, dropping a
/// first line that was cut part way. `true` when something was left unread.
fn read_end(path: &Path) -> Result<(String, bool), String> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((String::new(), false)),
        Err(e) => return Err(format!("{}", e.kind())),
    };
    if !meta.is_file() {
        return Err("it is not a regular file".to_owned());
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| format!("{}", e.kind()))?;
    let len = file.metadata().map_err(|e| format!("{}", e.kind()))?.len();
    let start = len.saturating_sub(READ_LIMIT);
    file.seek(SeekFrom::Start(start)).map_err(|e| format!("{}", e.kind()))?;
    let mut bytes = Vec::new();
    file.take(READ_LIMIT).read_to_end(&mut bytes).map_err(|e| format!("{}", e.kind()))?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0 {
        text = text.split_once('\n').map(|(_, rest)| rest.to_owned()).unwrap_or_default();
    }
    Ok((text, start > 0))
}

/// "2026-11-06 14:06:09" for a time in seconds since 1970, in UTC.
fn utc_time(at_unix: u64) -> String {
    let days = (at_unix / 86_400) as i64;
    let secs = at_unix % 86_400;
    // Howard Hinnant's days-to-civil, for the proleptic Gregorian calendar.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(lines: &[&str]) -> (tempfile::TempDir, hoststate::StatePaths) {
        let home = tempfile::tempdir().unwrap();
        let paths = hoststate::StatePaths { state: home.path().join("state"), overridden: true };
        hoststate::ensure_state(&paths).unwrap();
        std::fs::write(paths.daemon_audit(), lines.join("\n") + "\n").unwrap();
        (home, paths)
    }

    const LOG: &[&str] = &[
        r#"{"at_unix":1794095077,"action":"port_refused","project":"shop-1","subject":"3000","detail":"not approved"}"#,
        r#"{"at_unix":1794095233,"action":"person_declined","project":"shop-1","subject":"push","detail":"nobody answered within 120 seconds"}"#,
        r#"{"at_unix":1794095233,"action":"git_push_refused","project":"shop-1","subject":null,"detail":"not confirmed with Touch ID or your login password, so nothing was done: nobody answered within 120 seconds"}"#,
        r#"{"at_unix":1794095651,"action":"git_push_refused","project":"shop-1","subject":null,"detail":"pushing to main is not allowed from a sandbox. Make a branch with git switch -c NAME, then push that"}"#,
        r#"{"at_unix":1794095700,"action":"sandbox_started","project":"shop-1","subject":null,"detail":null}"#,
        r#"{"at_unix":1794096000,"action":"person_declined","project":"docs-2","subject":"start","detail":"you cancelled (LocalAuthentication -2)"}"#,
        r#"{"at_unix":1794097000,"action":"remote_fetch_failed","project":null,"subject":"git@codeberg.org:org/missing.git","detail":"The host's key is not in your known_hosts."}"#,
        "not json",
    ];

    #[test]
    fn refusals_are_listed_newest_first_and_the_rest_left_out() {
        let (_home, paths) = state_with(LOG);
        let view = problems(&paths, None);
        let what: Vec<&str> = view.rows.iter().map(|r| r.what.as_str()).collect();
        assert_eq!(what, ["Fetch git@codeberg.org:org/missing.git", "Start", "Push", "Push"]);
        assert_eq!(view.rows[0].project, None);
        assert_eq!(view.rows[0].time, "2026-11-08 00:16:40");
        assert!(view.rows[1].why.starts_with("Not confirmed: you cancelled"), "{:?}", view.rows[1]);
        assert!(view.notes.iter().any(|n| n.text.contains("1 lines")), "{:?}", view.notes);
    }

    #[test]
    fn a_project_sees_only_its_own() {
        let (_home, paths) = state_with(LOG);
        let root = paths.state.parent().unwrap().join("shop");
        std::fs::create_dir_all(&root).unwrap();
        let id = record_project(&paths, &root);
        let log: Vec<String> = LOG.iter().map(|l| l.replace("shop-1", &id)).collect();
        std::fs::write(paths.daemon_audit(), log.join("\n")).unwrap();
        let view = problems(&paths, Some(&root.to_string_lossy()));
        assert_eq!(view.rows.len(), 2, "{:?}", view.rows);
        assert!(view.rows.iter().all(|r| r.project.as_deref() == Some("shop")));
        assert!(view.rows[0].why.contains("main"));
    }

    /// Records a project the way the daemon does, and returns its identity.
    fn record_project(paths: &hoststate::StatePaths, root: &Path) -> String {
        let id = hoststate::project_id(root).unwrap();
        let dir = hoststate::prepare_project(paths, &id).unwrap();
        hoststate::ProjectRecord::touch(&dir, root, 1).unwrap();
        id.as_str().to_owned()
    }

    #[test]
    fn text_in_the_log_is_escaped_again() {
        let (_home, paths) = state_with(&[
            "{\"at_unix\":1,\"action\":\"git_push_refused\",\"project\":null,\"subject\":null,\"detail\":\"a\\u001b[2Jb\\nc\"}",
        ]);
        let view = problems(&paths, None);
        assert_eq!(view.rows.len(), 1);
        assert!(!view.rows[0].why.contains('\u{1b}') && !view.rows[0].why.contains('\n'), "{:?}", view.rows[0]);
        assert_eq!(view.rows[0].time, "1970-01-01 00:00:01");
    }

    #[test]
    fn no_log_means_nothing_refused() {
        let home = tempfile::tempdir().unwrap();
        let paths = hoststate::StatePaths { state: home.path().join("state"), overridden: true };
        let view = problems(&paths, None);
        assert!(view.rows.is_empty());
        assert_eq!(view.notes[0].text, "Nothing has been refused recently.");
    }

    #[test]
    fn only_the_end_of_a_long_log_is_read() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("log");
        let line = r#"{"at_unix":1,"action":"git_push_refused","project":null,"subject":null,"detail":"x"}"#;
        let many = (line.to_owned() + "\n").repeat((READ_LIMIT as usize / line.len()) + 10);
        std::fs::write(&path, many).unwrap();
        let (text, cut) = read_end(&path).unwrap();
        assert!(cut);
        assert!(text.lines().all(|l| l == line));
    }

    #[test]
    fn a_link_is_not_read() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("real"), "x").unwrap();
        std::os::unix::fs::symlink(home.path().join("real"), home.path().join("link")).unwrap();
        assert!(read_end(&home.path().join("link")).is_err());
    }
}
