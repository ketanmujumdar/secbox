//! `net blocked`: the hostnames a sandbox was refused, as the host's copy of the
//! engine's log records them.
//!
//! What this says about itself matters as much as the list. Most blocked traffic
//! leaves no record at all, so [`notes`] is shown every time, and the words are
//! the same here and in the full-screen interface.

use std::path::PathBuf;
use std::time::Duration;

use clap::Args;
use proto::{
    BlockedArgs, BlockedCursor, BlockedEvent, BlockedRecording, BlockedReport, LogTrust, Reply,
    Request,
};

use crate::client::{CallError, DaemonClient};
use crate::ui::{esc, paint, Tone};
use crate::{exit, project_dir, say, tell, Deps};

/// The most one request may take, so a stuck daemon cannot hold this for ever.
const REQUEST_LIMIT: Duration = Duration::from_secs(30);
const POLL_GAP: Duration = Duration::from_secs(1);

/// What is always true of this list.
pub(crate) const HOSTNAMES_ONLY: &str = "This shows blocked hostnames only. Raw IP, metadata and UDP attempts are blocked but not recorded, so an empty list doesn't mean nothing was tried.";

#[derive(Args)]
pub struct BlockedArgsCli {
    /// Project folder [default: the current folder]
    #[arg(long, value_name = "PATH")]
    project: Option<PathBuf>,
    /// Keep checking and show new events until Ctrl-C
    #[arg(short, long)]
    follow: bool,
}

/// How much a note should stand out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Weight {
    Plain,
    Warn,
    Alert,
}

pub(crate) fn not_recording_note() -> String {
    format!(
        "Nothing is being recorded for this sandbox, because it was not created at a log level that writes these. To record, it must be recreated with {bin} down --destroy, then {bin} up, which deletes its data.",
        bin = brand::CLI_BIN
    )
}

pub(crate) fn trust_note(trust: &LogTrust) -> Option<(Weight, String)> {
    match trust {
        LogTrust::NotRead => None,
        LogTrust::HostOnly => Some((
            Weight::Plain,
            format!(
                "The engine writes this record on the host. Measured on engine {}, the guest cannot write to its folder, but the guest does choose what it looks up.",
                engine::TESTED_VERSION
            ),
        )),
        LogTrust::Doubtful(reason) => Some((
            Weight::Alert,
            format!("This record is not trustworthy: {}.", esc(reason)),
        )),
    }
}

pub(crate) const CUT_OFF: &str = "Older events are cut off. The engine keeps about 50 MB of this log, and a busy or hostile sandbox can push events out of it within seconds.";

/// Everything that has to be said alongside a list, most important first.
pub(crate) fn notes(report: &BlockedReport) -> Vec<(Weight, String)> {
    let mut notes = Vec::new();
    match report.recording {
        BlockedRecording::NotRecording => notes.push((Weight::Alert, not_recording_note())),
        BlockedRecording::NoSandbox => notes.push((
            Weight::Warn,
            format!(
                "This project has no sandbox, so nothing is being recorded. One started with {} up records from then on.",
                brand::CLI_BIN
            ),
        )),
        BlockedRecording::Recording => {}
    }
    notes.push((Weight::Warn, HOSTNAMES_ONLY.to_owned()));
    notes.extend(trust_note(&report.trust));
    if report.cut_off {
        notes.push((Weight::Warn, CUT_OFF.to_owned()));
    }
    if report.omitted_domains > 0 {
        notes.push((
            Weight::Warn,
            format!(
                "{} older names are not listed, because there were too many.",
                report.omitted_domains
            ),
        ));
    }
    if report.rejected_lines > 0 {
        notes.push((
            Weight::Warn,
            format!(
                "{} log lines looked like blocked events but failed the check, so they are not listed.",
                report.rejected_lines
            ),
        ));
    }
    if report.unlisted_domains > 0 {
        notes.push((Weight::Warn, unlisted_note(report.unlisted_domains)));
    }
    notes
}

/// The sandbox chooses each name it looks up and can hide a secret in one, so
/// the daemon keeps names in memory only.
pub(crate) fn unlisted_note(count: u64) -> String {
    format!(
        "{count} names seen before the daemon last started are not listed. A sandbox chooses the names it looks up and can hide a secret in one, so names are never written to disk."
    )
}

/// One event as a line of text, the same everywhere it is shown.
pub(crate) fn row(event: &BlockedEvent) -> String {
    format!(
        "{}  {:>7}  {}{}",
        esc(&short_time(&event.time)),
        event.count,
        esc(&event.domain),
        if event.allowed { "  (approved since)" } else { "" }
    )
}

pub(crate) fn header() -> String {
    format!("{:<20}  {:>7}  {}", "TIME (UTC)", "COUNT", "HOSTNAME")
}

/// `2026-11-05T07:50:52.406636Z` as `2026-11-05 07:50:52`, padded to one width.
fn short_time(time: &str) -> String {
    let date = time.get(..10).unwrap_or(time);
    let clock = time.get(11..19).unwrap_or("");
    format!("{date} {clock:<9}")
}

/// Asks the daemon once. It has a limit, whoever is calling.
pub(crate) fn ask(
    client: &dyn DaemonClient,
    project: &str,
    after: Option<BlockedCursor>,
    limit: Duration,
) -> Result<BlockedReport, CallError> {
    match client.stream(
        Request::Blocked(BlockedArgs {
            project: project.to_owned(),
            after,
        }),
        &mut |_, _| true,
        Some(limit),
    ) {
        Ok(Reply::Blocked(report)) => Ok(report),
        Ok(_) => Err(CallError::Transport(
            "the daemon answered something else".to_owned(),
        )),
        Err(error) => Err(error),
    }
}

fn say_note(deps: &mut Deps, weight: Weight, text: &str) -> bool {
    let tone = match weight {
        Weight::Plain => Tone::Dim,
        Weight::Warn => Tone::Warn,
        Weight::Alert => Tone::Bad,
    };
    let line = paint(deps.color, tone, &esc(text));
    writeln!(deps.out, "{line}").is_ok() && deps.out.flush().is_ok()
}

fn say_events(deps: &mut Deps, events: &[BlockedEvent]) -> bool {
    for event in events {
        if writeln!(deps.out, "{}", row(event)).is_err() {
            return false;
        }
    }
    deps.out.flush().is_ok()
}

pub fn run(args: &BlockedArgsCli, deps: &mut Deps) -> i32 {
    let project = match project_dir(args.project.as_deref(), deps) {
        Ok(root) => root.to_string_lossy().into_owned(),
        Err(reason) => {
            tell!(deps, "net blocked: {reason}");
            return exit::FAILED;
        }
    };
    let client = deps.daemon;
    let report = match ask(client, &project, None, REQUEST_LIMIT) {
        Ok(report) => report,
        Err(error) => {
            tell!(deps, "net blocked: {}", error.for_terminal());
            return exit::FAILED;
        }
    };

    for (weight, text) in notes(&report) {
        say_note(deps, weight, &text);
    }
    say!(deps);
    let recording = report.recording == BlockedRecording::Recording;
    if recording {
        if report.events.is_empty() {
            say!(deps, "No blocked hostnames are recorded so far.");
        } else {
            say!(deps, "{}", paint(deps.color, Tone::Strong, &header()));
            say_events(deps, &report.events);
        }
    }
    if !args.follow {
        return exit::OK;
    }
    if !recording {
        tell!(deps, "net blocked: nothing can appear, so this is not following.");
        return exit::FAILED;
    }
    follow(client, &project, report.next, deps)
}

/// Asks again until Ctrl-C ends the process or nobody is reading. Each answer
/// says what it has to say again if it changed, since this can run for hours.
fn follow(
    client: &dyn DaemonClient,
    project: &str,
    mut after: Option<BlockedCursor>,
    deps: &mut Deps,
) -> i32 {
    let mut warned_timeout = false;
    let mut doubt_shown: Option<String> = None;
    loop {
        std::thread::sleep(POLL_GAP);
        let report = match ask(client, project, after, REQUEST_LIMIT) {
            Ok(report) => report,
            // A slow moment is not the end of following.
            Err(CallError::TimedOut) => {
                if !warned_timeout {
                    warned_timeout = true;
                    tell!(deps, "net blocked: the daemon did not answer in time. Still trying.");
                }
                continue;
            }
            Err(error) => {
                tell!(deps, "net blocked: {}", error.for_terminal());
                return exit::FAILED;
            }
        };
        warned_timeout = false;
        if report.recording != BlockedRecording::Recording {
            let (weight, text) = notes(&report).swap_remove(0);
            say_note(deps, weight, &text);
            return exit::FAILED;
        }
        if report.next.is_some() {
            after = report.next;
        }
        if let LogTrust::Doubtful(reason) = &report.trust {
            if doubt_shown.as_ref() != Some(reason) {
                doubt_shown = Some(reason.clone());
                let shown = trust_note(&report.trust).map(|(_, text)| text);
                if !say_note(deps, Weight::Alert, &shown.unwrap_or_default()) {
                    return exit::OK;
                }
            }
        }
        if report.cut_off && !say_note(deps, Weight::Warn, CUT_OFF) {
            return exit::OK;
        }
        if report.rejected_lines > 0 {
            let text = format!(
                "{} more log lines looked like blocked events but failed the check.",
                report.rejected_lines
            );
            if !say_note(deps, Weight::Warn, &text) {
                return exit::OK;
            }
        }
        // Whoever was reading has gone, as with a pipe into `head`.
        if !say_events(deps, &report.events) {
            return exit::OK;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(recording: BlockedRecording) -> BlockedReport {
        BlockedReport {
            recording,
            log_level: None,
            events: Vec::new(),
            total: 0,
            cut_off: false,
            omitted_domains: 0,
            rejected_lines: 0,
            unlisted_domains: 0,
            trust: LogTrust::NotRead,
            next: None,
        }
    }

    fn all(report: &BlockedReport) -> String {
        notes(report)
            .into_iter()
            .map(|(_, text)| text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn what_is_not_recorded_is_said_in_every_state() {
        for state in [
            BlockedRecording::Recording,
            BlockedRecording::NotRecording,
            BlockedRecording::NoSandbox,
        ] {
            let text = all(&report(state));
            assert!(text.contains("hostnames only"), "{text}");
            assert!(text.contains("Raw IP, metadata and UDP"), "{text}");
            assert!(text.contains("doesn't mean nothing was tried"), "{text}");
        }
    }

    #[test]
    fn not_recording_says_so_first_and_what_recreating_costs() {
        let notes = notes(&report(BlockedRecording::NotRecording));
        assert_eq!(notes[0].0, Weight::Alert);
        assert!(notes[0].1.contains("Nothing is being recorded"));
        assert!(notes[0].1.contains("down --destroy"));
        assert!(notes[0].1.contains(" up,"));
        assert!(notes[0].1.contains("deletes its data"));
    }

    #[test]
    fn a_doubtful_record_is_called_untrustworthy_and_a_sound_one_says_what_was_measured() {
        let mut r = report(BlockedRecording::Recording);
        r.trust = LogTrust::Doubtful("others can \u{1b}[31mwrite".to_owned());
        let text = all(&r);
        assert!(text.contains("not trustworthy"), "{text}");
        assert!(!text.contains('\u{1b}'), "{text}");

        r.trust = LogTrust::HostOnly;
        let text = all(&r);
        assert!(!text.contains("not trustworthy"), "{text}");
        assert!(text.contains(engine::TESTED_VERSION), "{text}");
    }

    #[test]
    fn anything_left_out_is_said() {
        let mut r = report(BlockedRecording::Recording);
        r.cut_off = true;
        r.omitted_domains = 3;
        r.rejected_lines = 2;
        r.unlisted_domains = 4;
        let text = all(&r);
        assert!(text.contains("cut off"), "{text}");
        assert!(text.contains("3 older names"), "{text}");
        assert!(text.contains("2 log lines"), "{text}");
        assert!(text.contains("4 names seen before the daemon last started"), "{text}");
    }

    #[test]
    fn a_row_shows_the_time_the_count_and_the_name_and_cannot_carry_a_control_code() {
        let line = row(&BlockedEvent {
            time: "2026-11-05T07:50:52.406636Z".to_owned(),
            domain: "a\u{1b}[2J.example.net".to_owned(),
            count: 12,
            allowed: false,
        });
        assert!(line.starts_with("2026-11-05 07:50:52 "), "{line}");
        assert!(line.contains("  12  "), "{line}");
        assert!(!line.contains('\u{1b}'), "{line}");
        assert!(!line.contains("approved"), "{line}");
    }

    #[test]
    fn a_name_approved_since_says_so() {
        let line = row(&BlockedEvent {
            time: "2026-11-05T07:50:52.406636Z".to_owned(),
            domain: "example.com".to_owned(),
            count: 2,
            allowed: true,
        });
        assert!(line.ends_with("example.com  (approved since)"), "{line}");
    }
}
