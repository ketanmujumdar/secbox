//! Reading the blocked hostnames the engine wrote down.
//!
//! The engine logs a refused name lookup only at a raised log level, in a file
//! on the host. What was measured on engine v0.7.13 shapes all of this:
//!
//! - The line is exact, and a name in it is in DNS text form, so a newline or a
//!   space in a crafted name arrives as `\012` or `\040`. Nothing else is
//!   accepted, and a line that nearly matches is counted rather than guessed at.
//! - Attempts to raw addresses, the metadata address and UDP are refused without
//!   a line at any level. An empty list here does not mean nothing was tried.
//! - The log is renamed at about 10 MB, to `.1` up to `.4`, and the oldest is
//!   deleted. A sandbox making a lot of noise can push a record out within
//!   seconds, because file activity in the guest is logged too.
//! - The guest's two file shares are the engine's guest state and its bootstrap
//!   root. Neither holds the logs folder, and nothing is read from either here.
//!
//! The file is still treated as hostile. Each folder on the way to it is checked
//! without following a link, and the file is opened so that a link or a pipe put
//! in its place fails rather than being read.

use std::collections::BTreeMap;
use std::fs::{File, Metadata, OpenOptions};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use engine::{argv, EngineLocation, SandboxName};
use proto::{
    text, ApiError, BlockedArgs, BlockedCursor, BlockedEvent, BlockedRecording, BlockedReport,
    BlockedSummary, LogTrust, SandboxState,
};

use crate::run::status_of;
use crate::settings::SandboxRecord;
use crate::{internal, refused, unsupported, Project, Service};

/// The most read in one request, across the current file and the renamed ones.
/// It is above what the engine keeps, so it only bites if that ever changes.
const MAX_READ_BYTES: u64 = 64 * 1024 * 1024;
/// The most rows sent. The newest are kept, and a count says how many were not.
const MAX_EVENTS: usize = 1000;
/// How many renamed files the engine keeps. When the last exists, older ones may
/// already be gone.
const ENGINE_KEEPS_RENAMED: usize = 4;
/// How far along the renamed files this looks, in case the engine keeps more.
const MAX_RENAMED: usize = 9;
const LOG_FILE: &str = "runtime.log";

/// `2026-11-05T07:50:40.358660Z`, the only time format accepted.
const TIME_BYTES: usize = 27;
/// Everything on the line between the time and the name.
const LINE_MIDDLE: &[u8] = b" DEBUG microsandbox_network::engine::dns::forwarder: DNS query denied by network policy domain=";
/// A DNS name is at most 255 bytes, and each takes at most four as text.
const MAX_DOMAIN_BYTES: usize = 1020;
/// A line with no end longer than this will never be one of ours.
const MAX_LINE_BYTES: u64 = 2048;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LogLine<'a> {
    Event { time: &'a str, domain: &'a str },
    /// In the place of the exact line, but with a time or name that fails.
    Rejected,
    Other,
}

pub(crate) fn parse_line(line: &[u8]) -> LogLine<'_> {
    let Some(domain) = line
        .get(TIME_BYTES..)
        .and_then(|rest| rest.strip_prefix(LINE_MIDDLE))
    else {
        return LogLine::Other;
    };
    let time = &line[..TIME_BYTES];
    if !valid_time(time) || !valid_domain(domain) {
        return LogLine::Rejected;
    }
    match (std::str::from_utf8(time), std::str::from_utf8(domain)) {
        (Ok(time), Ok(domain)) => LogLine::Event { time, domain },
        _ => LogLine::Rejected,
    }
}

fn valid_time(time: &[u8]) -> bool {
    let number = |range: std::ops::Range<usize>| -> Option<u32> {
        time.get(range)?
            .iter()
            .try_fold(0u32, |n, b| b.is_ascii_digit().then(|| n * 10 + u32::from(b - b'0')))
    };
    time.len() == TIME_BYTES
        && [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':'), (19, b'.'), (26, b'Z')]
            .iter()
            .all(|(at, wanted)| time[*at] == *wanted)
        && number(0..4).is_some()
        && number(5..7).is_some_and(|month| (1..=12).contains(&month))
        && number(8..10).is_some_and(|day| (1..=31).contains(&day))
        && number(11..13).is_some_and(|hour| hour <= 23)
        && number(14..16).is_some_and(|minute| minute <= 59)
        && number(17..19).is_some_and(|second| second <= 60)
        && number(20..26).is_some()
}

/// A name in DNS text form: letters, digits, `-` and `_`, labels between dots,
/// and `\` with three octal digits or one punctuation mark for anything else.
/// So nothing accepted has a space, a control code or a non-ASCII byte, and the
/// backslashes stay visible, which keeps it from passing as another name.
fn valid_domain(domain: &[u8]) -> bool {
    if domain.is_empty() || domain.len() > MAX_DOMAIN_BYTES {
        return false;
    }
    if domain == b"." {
        return true;
    }
    let mut at = 0;
    let mut label_len = 0;
    while at < domain.len() {
        match domain[at] {
            b'.' => {
                if label_len == 0 {
                    return false;
                }
                label_len = 0;
                at += 1;
            }
            b'\\' => {
                let Some(step) = escape_len(&domain[at + 1..]) else {
                    return false;
                };
                label_len += 1;
                at += step + 1;
            }
            b if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' => {
                label_len += 1;
                at += 1;
            }
            _ => return false,
        }
    }
    true
}

/// How many bytes follow a backslash, if they make a proper escape.
fn escape_len(after: &[u8]) -> Option<usize> {
    match after {
        // The engine writes these in octal: a space is `\040`, and `é` is `\303\251`.
        [b'0'..=b'3', b'0'..=b'7', b'0'..=b'7', ..] => Some(3),
        [mark, ..] if mark.is_ascii_punctuation() => Some(1),
        _ => None,
    }
}

/// Events grouped by name. A resolver can vary the case of a name it asks for,
/// so the case is not part of what makes two names the same.
#[derive(Default)]
struct Groups {
    by_domain: BTreeMap<String, (String, u64)>,
    total: u64,
    rejected: u64,
}

impl Groups {
    fn add(&mut self, line: &[u8]) {
        match parse_line(line) {
            LogLine::Event { time, domain } => {
                self.total += 1;
                let entry = self
                    .by_domain
                    .entry(domain.to_ascii_lowercase())
                    .or_insert_with(|| (String::new(), 0));
                entry.1 += 1;
                // Threads in the engine can write slightly out of order.
                if time > entry.0.as_str() {
                    time.clone_into(&mut entry.0);
                }
            }
            LogLine::Rejected => self.rejected += 1,
            LogLine::Other => {}
        }
    }

    /// The newest last, and no more than `keep` of them, with how many were left.
    fn into_events(self, keep: usize) -> (Vec<BlockedEvent>, u64) {
        let mut events: Vec<BlockedEvent> = self
            .by_domain
            .into_iter()
            .map(|(domain, (time, count))| BlockedEvent {
                time,
                domain: text::escape(&domain),
                count,
            })
            .collect();
        events.sort_by(|a, b| a.time.cmp(&b.time).then_with(|| a.domain.cmp(&b.domain)));
        let left_out = events.len().saturating_sub(keep);
        (events.split_off(left_out), left_out as u64)
    }
}

/// What came out of the log files.
#[derive(Default)]
struct LogRead {
    groups: Groups,
    cut_off: bool,
    next: Option<BlockedCursor>,
    trust: Option<String>,
}

struct LogFile {
    file: File,
    id: u64,
    size: u64,
}

/// Opens a log file for reading, or says it is not there. A link or a pipe at
/// this path fails here, whatever it was replaced with since anything looked.
fn open_log(path: &Path, shown: &str, me: u32) -> Result<Option<LogFile>, String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path);
    let file = match file {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) if e.raw_os_error() == Some(nix::libc::ELOOP) => {
            return Err(format!("{shown} is a symbolic link"))
        }
        Err(e) => return Err(format!("cannot open {shown}: {}", e.kind())),
    };
    let meta = file
        .metadata()
        .map_err(|e| format!("cannot inspect {shown}: {}", e.kind()))?;
    if !meta.is_file() {
        return Err(format!("{shown} is not a regular file"));
    }
    if meta.uid() != me {
        return Err(format!("{shown} belongs to another user"));
    }
    if meta.nlink() != 1 {
        return Err(format!("{shown} has more than one name on disk"));
    }
    Ok(Some(LogFile {
        id: meta.ino(),
        size: meta.len(),
        file,
    }))
}

/// Checks the folders down to the logs without following anything. `None` means
/// one is not there yet, which is not a fault.
fn check_dirs(dirs: &[PathBuf], me: u32) -> Result<Option<Vec<Metadata>>, String> {
    let mut seen = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let shown = dir.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let meta = match std::fs::symlink_metadata(dir) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("cannot inspect {shown}: {}", e.kind())),
        };
        if meta.file_type().is_symlink() {
            return Err(format!("{shown} is a symbolic link"));
        }
        if !meta.is_dir() {
            return Err(format!("{shown} is not a folder"));
        }
        if meta.uid() != me {
            return Err(format!("{shown} belongs to another user"));
        }
        seen.push(meta);
    }
    Ok(Some(seen))
}

/// Reads part of a file, stopping early if it turns out shorter.
fn read_range(file: &File, start: u64, end: u64) -> Result<Vec<u8>, String> {
    let wanted = usize::try_from(end - start).map_err(|_| "too much to read".to_owned())?;
    let mut bytes = vec![0; wanted];
    let mut filled = 0;
    while filled < wanted {
        match file.read_at(&mut bytes[filled..], start + filled as u64) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(e) => return Err(format!("cannot read the log: {}", e.kind())),
        }
    }
    bytes.truncate(filled);
    Ok(bytes)
}

/// Feeds the whole lines between two offsets to `groups`, and returns where the
/// next read should start.
///
/// A line counts only when its start is known to be the start of a line, so the
/// tail of a long line, cut anywhere, cannot pass for one. The last line is left
/// for later if it has no end yet, unless it is already too long to be ours.
fn read_lines(log: &LogFile, start: u64, groups: &mut Groups) -> Result<u64, String> {
    let from = start.saturating_sub(1);
    let bytes = read_range(&log.file, from, log.size)?;
    let mut rest: &[u8] = &bytes;
    let mut at = from;
    if start > 0 {
        let skip = rest.iter().position(|b| *b == b'\n').map_or(rest.len(), |i| i + 1);
        rest = &rest[skip..];
        at += skip as u64;
    }
    while let Some(end) = rest.iter().position(|b| *b == b'\n') {
        groups.add(&rest[..end]);
        rest = &rest[end + 1..];
        at += end as u64 + 1;
    }
    if rest.len() as u64 > MAX_LINE_BYTES {
        return Ok(at + rest.len() as u64);
    }
    Ok(at)
}

/// Reads the log files, newest backwards, up to `limit` bytes in all.
fn read_log(
    dirs: &[PathBuf],
    after: Option<BlockedCursor>,
    limit: u64,
) -> Result<LogRead, String> {
    let me = nix::unistd::Uid::current().as_raw();
    let mut read = LogRead {
        next: after,
        ..LogRead::default()
    };
    let Some(before) = check_dirs(dirs, me)? else {
        return Ok(read);
    };
    if before.iter().any(|meta| meta.mode() & 0o022 != 0) {
        read.trust = Some(
            "a folder on the way to it can be written by other accounts on this machine".to_owned(),
        );
    }
    let logs = dirs.last().ok_or("no log folder was named")?;

    // The newest first. The current file can be missing for a moment while the
    // engine renames it, but a gap further along means there are no more.
    let mut files = Vec::new();
    let mut current = None;
    for index in 0..=MAX_RENAMED {
        let name = if index == 0 {
            LOG_FILE.to_owned()
        } else {
            format!("{LOG_FILE}.{index}")
        };
        match open_log(&logs.join(&name), &name, me)? {
            Some(log) => {
                if index == 0 {
                    current = Some(log.id);
                }
                files.push(log);
            }
            None if index == 0 => {}
            None => break,
        }
    }

    // Decide what to read from each, then read the oldest first.
    let mut budget = limit;
    let mut parts = Vec::new();
    let mut found = false;
    for (index, log) in files.iter().enumerate() {
        let floor = match after {
            Some(cursor) if cursor.file == log.id && cursor.offset <= log.size => {
                found = true;
                cursor.offset
            }
            _ => 0,
        };
        let left = log.size - floor;
        let taken = left.min(budget);
        budget -= taken;
        parts.push((index, log.size - taken));
        if taken < left {
            read.cut_off = true;
            break;
        }
        if found {
            break;
        }
    }
    match after {
        Some(_) if !found => read.cut_off = true,
        None if files.len() > ENGINE_KEEPS_RENAMED => read.cut_off = true,
        _ => {}
    }

    for (index, start) in parts.into_iter().rev() {
        let log = &files[index];
        let reached = read_lines(log, start, &mut read.groups)?;
        if Some(log.id) == current {
            read.next = Some(BlockedCursor {
                file: log.id,
                offset: reached,
            });
        }
    }

    // A folder swapped for another while this ran means none of it can be
    // believed, whatever it said.
    match check_dirs(dirs, me)? {
        Some(after) if same_folders(&before, &after) => Ok(read),
        _ => Err("the folders changed while they were being read".to_owned()),
    }
}

fn same_folders(a: &[Metadata], b: &[Metadata]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| hoststate::same_directory(a, b))
}

/// Whether a level means the engine writes these lines. Measured: `debug` does,
/// and the default writes none of them.
fn level_records(level: &str) -> bool {
    matches!(level, "debug" | "trace")
}

/// What the host's own record says about this sandbox. A sandbox with no level
/// on record is taken to be recording nothing, and none is ever written for it.
fn recording(
    project: &Project,
    state: SandboxState,
) -> Result<(BlockedRecording, Option<String>), ApiError> {
    if state == SandboxState::None {
        return Ok((BlockedRecording::NoSandbox, None));
    }
    let level = SandboxRecord::load(&project.dir)
        .map_err(internal)?
        .and_then(|record| record.log_level);
    let answer = match level.as_deref() {
        Some(level) if level_records(level) => BlockedRecording::Recording,
        _ => BlockedRecording::NotRecording,
    };
    Ok((answer, level))
}

fn log_dirs(service: &Service, name: &SandboxName) -> Result<Vec<PathBuf>, ApiError> {
    let location: EngineLocation = service
        .deps()
        .engine()
        .location()
        .ok_or_else(|| unsupported("this engine has no log files to read"))?;
    Ok(location.sandbox_log_dirs(name))
}

pub fn report(
    service: &Service,
    project: &Project,
    args: &BlockedArgs,
) -> Result<BlockedReport, ApiError> {
    let state = status_of(service, project)?;
    let (recording, log_level) = recording(project, state)?;
    if recording != BlockedRecording::Recording {
        return Ok(BlockedReport {
            recording,
            log_level,
            events: Vec::new(),
            total: 0,
            cut_off: false,
            omitted_domains: 0,
            rejected_lines: 0,
            trust: LogTrust::NotRead,
            next: None,
        });
    }
    let dirs = log_dirs(service, &project.sandbox)?;
    let read = read_log(&dirs, args.after, MAX_READ_BYTES)
        .map_err(|reason| refused(format!("the engine's log was not read: {reason}")))?;
    let total = read.groups.total;
    let rejected_lines = read.groups.rejected;
    let (mut events, omitted_domains) = read.groups.into_events(MAX_EVENTS);
    // A thief can put a secret in the name it looks up, so the names are
    // masked like log text before they leave the daemon.
    let masker = crate::mask::Masker::for_project(service, project)?;
    for event in &mut events {
        event.domain = masker.mask_hostname(&event.domain);
    }
    Ok(BlockedReport {
        recording,
        log_level,
        events,
        total,
        cut_off: read.cut_off,
        omitted_domains,
        rejected_lines,
        trust: read.trust.map_or(LogTrust::HostOnly, LogTrust::Doubtful),
        next: read.next,
    })
}

/// The short form for `status`, which must keep working whatever the log is
/// doing, so a problem here is part of the answer rather than a failure.
pub(crate) fn summary(service: &Service, project: &Project, state: SandboxState) -> BlockedSummary {
    let mut summary = BlockedSummary {
        recording: BlockedRecording::NotRecording,
        total: 0,
        domains: 0,
        cut_off: false,
        trust: LogTrust::NotRead,
        problem: None,
    };
    match recording(project, state) {
        Ok((recording, _)) => summary.recording = recording,
        Err(error) => {
            summary.problem = Some(error.message);
            return summary;
        }
    }
    if summary.recording != BlockedRecording::Recording {
        return summary;
    }
    let dirs = match log_dirs(service, &project.sandbox) {
        Ok(dirs) => dirs,
        Err(error) => {
            summary.problem = Some(error.message);
            return summary;
        }
    };
    match read_log(&dirs, None, MAX_READ_BYTES) {
        Ok(read) => {
            summary.total = read.groups.total;
            summary.domains = read.groups.by_domain.len() as u64;
            summary.cut_off = read.cut_off;
            summary.trust = read.trust.map_or(LogTrust::HostOnly, LogTrust::Doubtful);
        }
        Err(reason) => summary.problem = Some(text::escape_short(&reason, 200)),
    }
    summary
}

/// The level new sandboxes are created at, as the host records it.
pub(crate) fn created_level() -> String {
    argv::CREATE_LOG_LEVEL.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::{symlink, PermissionsExt};

    const TIME: &str = "2026-11-05T07:50:52.406636Z";

    fn line(time: &str, domain: &str) -> String {
        format!(
            "{time} DEBUG microsandbox_network::engine::dns::forwarder: DNS query denied by network policy domain={domain}"
        )
    }

    fn event(line: &str) -> Option<(&str, &str)> {
        match parse_line(line.as_bytes()) {
            LogLine::Event { time, domain } => Some((time, domain)),
            _ => None,
        }
    }

    /// Lines copied from a real sandbox on engine v0.7.13, where the guest sent
    /// crafted queries by hand.
    #[test]
    fn real_lines_are_read_with_the_engines_own_escapes_left_visible() {
        for domain in [
            "qzplain.example.net",
            "_dmarc.example.net",
            "qzsingle",
            "QzUpPeR.Example.NET",
            r"a\.b.example.net",
            r"x\033\[31mred.example.net",
            r"x\0122026-11-05T00\:00\:00\.000000Z\040DEBUG\040a\:\040b\040domain\=z.example.net",
            r"a\040domain\=fake.example.net",
            r"a\000b.example.net",
            r#"a\"b.example.net"#,
            r"\303\251.example.net",
        ] {
            let text = line(TIME, domain);
            assert_eq!(event(&text), Some((TIME, domain)), "{domain}");
            assert_eq!(proto::text::escape(domain), domain, "must already be safe");
            assert!(!domain.contains(' '));
        }
    }

    #[test]
    fn every_other_real_kind_of_line_is_not_an_event() {
        for other in [
            "2026-11-05T07:49:44.052166Z  INFO microsandbox_runtime::runner::vm: sandbox starting sandbox=tb1",
            "2026-11-05T07:50:18.358159Z DEBUG msb_krun_devices::virtio::fs::server: opcode: 1",
            "2026-11-05T07:50:18.358159Z DEBUG microsandbox_network::engine::dns::forwarder: DNS query family is inactive",
            "",
            "short",
        ] {
            assert_eq!(parse_line(other.as_bytes()), LogLine::Other, "{other}");
        }
    }

    /// The exact line has to start at the start, so text from anywhere else that
    /// merely contains it does not count.
    #[test]
    fn the_exact_line_in_the_middle_of_another_is_not_an_event() {
        let inner = line(TIME, "forged.example.net");
        for text in [
            format!("x{inner}"),
            format!(" {inner}"),
            format!("{TIME} DEBUG sqlx::query: summary=\"{inner}\""),
        ] {
            assert_eq!(event(&text), None, "{text}");
        }
    }

    #[test]
    fn a_crafted_name_cannot_add_a_field_or_a_line() {
        for domain in [
            "a.example.net domain=fake.example.net",
            "a.example.net time=2020",
            "a b.example.net",
            "a\u{1b}[31m.example.net",
            "a\u{7}.example.net",
            "a\rb.example.net",
            "a.example.net\r",
            "a\u{202e}b.example.net",
            "caf\u{e9}.example.net",
            "a\tb",
            "a;b.example.net",
            "a=b.example.net",
            "..example.net",
            ".example.net",
            "a..net",
            "",
            r"a\",
            r"a\9",
            r"a\999b",
            r"a\400b",
            r"a\08b",
            r"a\ b",
        ] {
            let text = line(TIME, domain);
            assert_eq!(parse_line(text.as_bytes()), LogLine::Rejected, "{domain:?}");
        }
    }

    #[test]
    fn bytes_that_are_not_text_are_refused() {
        let mut bytes = line(TIME, "a").into_bytes();
        bytes.extend_from_slice(b"\xff\xfe.example.net");
        assert_eq!(parse_line(&bytes), LogLine::Rejected);
    }

    #[test]
    fn a_time_that_is_not_exactly_the_format_is_refused() {
        for time in [
            "2026-11-05 07:50:52.406636Z",
            "2026-13-05T07:50:52.406636Z",
            "2026-11-32T07:50:52.406636Z",
            "2026-11-05T24:50:52.406636Z",
            "2026-11-05T07:60:52.406636Z",
            "2026-11-05T07:50:52.406636X",
            "abcd-11-05T07:50:52.406636Z",
            "\u{1b}026-11-05T07:50:52.406636Z",
        ] {
            let text = line(time, "a.example.net");
            assert_eq!(event(&text), None, "{time:?}");
        }
    }

    #[test]
    fn a_huge_name_is_refused_and_the_largest_real_one_is_not() {
        assert_eq!(
            parse_line(line(TIME, &"a".repeat(MAX_DOMAIN_BYTES + 1)).as_bytes()),
            LogLine::Rejected
        );
        assert!(event(&line(TIME, &"a".repeat(MAX_DOMAIN_BYTES))).is_some());
    }

    struct Logs {
        _temp: tempfile::TempDir,
        dirs: Vec<PathBuf>,
    }

    impl Logs {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let location = EngineLocation {
                binary: PathBuf::from("/nonexistent"),
                home: temp.path().join("msb"),
            };
            let name = SandboxName::new("demo").unwrap();
            let dirs = location.sandbox_log_dirs(&name);
            std::fs::create_dir_all(dirs.last().unwrap()).unwrap();
            for dir in &dirs {
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            Self { _temp: temp, dirs }
        }

        fn logs(&self) -> &Path {
            self.dirs.last().unwrap()
        }

        fn sandbox(&self) -> &Path {
            &self.dirs[2]
        }

        fn write(&self, name: &str, text: &str) {
            std::fs::write(self.logs().join(name), text).unwrap();
        }

        fn append(&self, name: &str, text: &str) {
            let mut file = OpenOptions::new()
                .append(true)
                .create(true)
                .open(self.logs().join(name))
                .unwrap();
            file.write_all(text.as_bytes()).unwrap();
        }

        fn read(&self) -> LogRead {
            read_log(&self.dirs, None, MAX_READ_BYTES).unwrap()
        }

        fn read_after(&self, after: Option<BlockedCursor>) -> LogRead {
            read_log(&self.dirs, after, MAX_READ_BYTES).unwrap()
        }

        fn refused(&self) -> String {
            match read_log(&self.dirs, None, MAX_READ_BYTES) {
                Ok(_) => panic!("this should have been refused"),
                Err(reason) => reason,
            }
        }
    }

    fn at(second: u32) -> String {
        format!("2026-11-05T07:50:{second:02}.000000Z")
    }

    fn rows(read: LogRead) -> Vec<(String, String, u64)> {
        read.groups
            .into_events(MAX_EVENTS)
            .0
            .into_iter()
            .map(|e| (e.time, e.domain, e.count))
            .collect()
    }

    #[test]
    fn repeats_are_grouped_by_name_whatever_their_case_and_the_newest_is_last() {
        let logs = Logs::new();
        let text = [
            line(&at(1), "old.example.net"),
            line(&at(2), "new.example.net"),
            "2026-11-05T07:50:18.358159Z DEBUG msb_krun_hvf: EC_DATAABORT 1".to_owned(),
            line(&at(3), "OLD.Example.NET"),
            line(&at(4), "new.example.net"),
            line(&at(3), "new.example.net"),
        ]
        .join("\n");
        logs.write(LOG_FILE, &format!("{text}\n"));
        let read = logs.read();
        assert_eq!(read.groups.total, 5);
        assert_eq!(
            rows(read),
            [
                (at(3), "old.example.net".to_owned(), 2),
                (at(4), "new.example.net".to_owned(), 3),
            ]
        );
    }

    #[test]
    fn a_rejected_line_is_counted_and_not_shown() {
        let logs = Logs::new();
        let bad = line(&at(1), "a b.example.net");
        let good = line(&at(2), "ok.example.net");
        logs.write(LOG_FILE, &format!("{bad}\n{good}\n"));
        let read = logs.read();
        assert_eq!(read.groups.rejected, 1);
        assert_eq!(rows(read), [(at(2), "ok.example.net".to_owned(), 1)]);
    }

    #[test]
    fn a_newline_ends_a_line_so_only_a_whole_real_line_can_ever_count() {
        let logs = Logs::new();
        // What the file would hold if an engine wrote a raw newline from a name.
        let text = format!(
            "{}\n{}\n",
            line(&at(1), "x"),
            "2026 DEBUG a: b domain=z.example.net",
        );
        logs.write(LOG_FILE, &text);
        assert_eq!(rows(logs.read()), [(at(1), "x".to_owned(), 1)]);
    }

    #[test]
    fn a_truncated_last_line_waits_until_it_is_whole() {
        let logs = Logs::new();
        let whole = line(&at(1), "whole.example.net");
        let later = line(&at(2), "later.example.net");
        let (start, end) = later.split_at(later.len() - 10);
        logs.write(LOG_FILE, &format!("{whole}\n{start}"));
        let first = logs.read();
        let cursor = first.next;
        assert_eq!(rows(first), [(at(1), "whole.example.net".to_owned(), 1)]);

        logs.append(LOG_FILE, &format!("{end}\n"));
        assert_eq!(
            rows(logs.read_after(cursor)),
            [(at(2), "later.example.net".to_owned(), 1)]
        );
    }

    #[test]
    fn following_returns_only_what_is_new_and_nothing_twice() {
        let logs = Logs::new();
        logs.write(LOG_FILE, &format!("{}\n", line(&at(1), "a.example.net")));
        let first = logs.read();
        let cursor = first.next;
        assert_eq!(logs.read_after(cursor).groups.total, 0);
        logs.append(LOG_FILE, &format!("{}\n", line(&at(2), "b.example.net")));
        let second = logs.read_after(cursor);
        assert!(!second.cut_off);
        assert_eq!(rows(second), [(at(2), "b.example.net".to_owned(), 1)]);
    }

    /// The engine renames the file at about 10 MB. A follower must still see what
    /// was written just before that, in the file it was reading.
    #[test]
    fn following_carries_on_across_the_engine_renaming_the_file() {
        let logs = Logs::new();
        logs.write(LOG_FILE, &format!("{}\n", line(&at(1), "a.example.net")));
        let cursor = logs.read().next;
        logs.append(LOG_FILE, &format!("{}\n", line(&at(2), "b.example.net")));
        std::fs::rename(logs.logs().join(LOG_FILE), logs.logs().join("runtime.log.1")).unwrap();
        logs.write(LOG_FILE, &format!("{}\n", line(&at(3), "c.example.net")));
        let read = logs.read_after(cursor);
        assert!(!read.cut_off);
        assert_eq!(
            rows(read),
            [
                (at(2), "b.example.net".to_owned(), 1),
                (at(3), "c.example.net".to_owned(), 1),
            ]
        );
    }

    #[test]
    fn a_follower_whose_file_has_gone_is_told_something_may_be_missing() {
        let logs = Logs::new();
        logs.write(LOG_FILE, &format!("{}\n", line(&at(1), "a.example.net")));
        let cursor = logs.read().next;
        std::fs::remove_file(logs.logs().join(LOG_FILE)).unwrap();
        logs.write(LOG_FILE, &format!("{}\n", line(&at(2), "b.example.net")));
        let read = logs.read_after(cursor.map(|c| BlockedCursor { file: c.file + 1_000_000, ..c }));
        assert!(read.cut_off);
        assert_eq!(rows(read), [(at(2), "b.example.net".to_owned(), 1)]);
    }

    #[test]
    fn the_renamed_files_are_read_too_and_the_last_one_says_older_may_be_gone() {
        let logs = Logs::new();
        logs.write(LOG_FILE, &format!("{}\n", line(&at(5), "new.example.net")));
        logs.write("runtime.log.1", &format!("{}\n", line(&at(4), "old.example.net")));
        let read = logs.read();
        assert!(!read.cut_off);
        assert_eq!(read.groups.total, 2);

        for index in 2..=ENGINE_KEEPS_RENAMED {
            logs.write(&format!("runtime.log.{index}"), &format!("{}\n", line(&at(3), "older.example.net")));
        }
        assert!(logs.read().cut_off);
    }

    #[test]
    fn the_bound_keeps_the_newest_and_says_it_cut_something_off() {
        let logs = Logs::new();
        let mut text = String::new();
        for second in 0..50 {
            text.push_str(&line(&at(second), &format!("n{second}.example.net")));
            text.push('\n');
        }
        logs.write(LOG_FILE, &text);
        let one_line = line(&at(0), "n0.example.net").len() as u64 + 1;
        let read = read_log(&logs.dirs, None, one_line * 3 + one_line / 2).unwrap();
        assert!(read.cut_off);
        assert_eq!(read.groups.total, 3, "the line cut in half must not count");
        assert_eq!(rows(read).last().unwrap().1, "n49.example.net");
    }

    #[test]
    fn the_bound_counts_across_files_and_stops_at_the_newest() {
        let logs = Logs::new();
        let new = line(&at(5), "new.example.net");
        logs.write(LOG_FILE, &format!("{new}\n"));
        logs.write("runtime.log.1", &format!("{}\n", line(&at(4), "old.example.net")));
        let read = read_log(&logs.dirs, None, new.len() as u64 + 1).unwrap();
        assert!(read.cut_off);
        assert_eq!(rows(read), [(at(5), "new.example.net".to_owned(), 1)]);
    }

    /// The tail of a line cut off by the bound is exactly what a hostile long
    /// line would aim at, so it has to be thrown away whatever it looks like.
    #[test]
    fn the_end_of_a_huge_line_cannot_pass_for_a_line_of_its_own() {
        let logs = Logs::new();
        let forged = line(&at(9), "forged.example.net");
        let huge = format!("{}{forged}\n", "x".repeat(5000));
        logs.write(LOG_FILE, &huge);
        let read = read_log(&logs.dirs, None, forged.len() as u64 + 1).unwrap();
        assert_eq!(read.groups.total, 0);
        assert!(read.cut_off);
    }

    #[test]
    fn a_huge_line_with_no_end_is_stepped_over_when_following() {
        let logs = Logs::new();
        logs.write(LOG_FILE, &"x".repeat(MAX_LINE_BYTES as usize + 10));
        let first = logs.read();
        let cursor = first.next.unwrap();
        assert_eq!(cursor.offset, MAX_LINE_BYTES + 10);

        let forged = line(&at(9), "forged.example.net");
        let good = line(&at(10), "good.example.net");
        logs.append(LOG_FILE, &format!("{forged}\n{good}\n"));
        assert_eq!(
            rows(logs.read_after(Some(cursor))),
            [(at(10), "good.example.net".to_owned(), 1)]
        );
    }

    #[test]
    fn nothing_there_yet_is_an_empty_answer_and_not_a_fault() {
        let temp = tempfile::tempdir().unwrap();
        let dirs = vec![temp.path().join("missing"), temp.path().join("missing/logs")];
        let read = read_log(&dirs, None, MAX_READ_BYTES).unwrap();
        assert_eq!(read.groups.total, 0);

        let logs = Logs::new();
        assert_eq!(logs.read().groups.total, 0);
    }

    #[test]
    fn a_link_at_the_log_is_refused_even_a_dangling_one() {
        let logs = Logs::new();
        let target = logs.sandbox().join("target");
        std::fs::write(&target, line(&at(1), "planted.example.net") + "\n").unwrap();
        symlink(&target, logs.logs().join(LOG_FILE)).unwrap();
        assert!(logs.refused().contains("symbolic link"));

        std::fs::remove_file(logs.logs().join(LOG_FILE)).unwrap();
        symlink("/nonexistent/anywhere", logs.logs().join(LOG_FILE)).unwrap();
        assert!(logs.refused().contains("symbolic link"));
    }

    #[test]
    fn a_link_at_a_renamed_file_is_refused() {
        let logs = Logs::new();
        logs.write(LOG_FILE, "");
        symlink("/etc/passwd", logs.logs().join("runtime.log.1")).unwrap();
        assert!(logs.refused().contains("symbolic link"));
    }

    #[test]
    fn a_link_at_any_folder_on_the_way_is_refused() {
        for index in 0..4 {
            let logs = Logs::new();
            let dir = &logs.dirs[index];
            let moved = dir.with_file_name("moved-away");
            std::fs::rename(dir, &moved).unwrap();
            symlink(&moved, dir).unwrap();
            assert!(logs.refused().contains("symbolic link"), "{index}");
        }
    }

    /// The guest can make any link it likes under its own state, which becomes a
    /// real link on the host. A logs folder pointing in there has to fail, and
    /// nothing in there is ever a place this reads.
    #[test]
    fn nothing_under_the_guests_own_state_is_followed_or_read() {
        let logs = Logs::new();
        let guest = logs.sandbox().join("guest-state");
        std::fs::create_dir_all(guest.join("logs")).unwrap();
        std::fs::write(
            guest.join("logs").join(LOG_FILE),
            line(&at(1), "planted.example.net") + "\n",
        )
        .unwrap();
        // Sitting there alone it changes nothing.
        assert_eq!(logs.read().groups.total, 0);

        std::fs::remove_dir(logs.logs()).unwrap();
        symlink(guest.join("logs"), logs.logs()).unwrap();
        assert!(logs.refused().contains("symbolic link"));
    }

    #[test]
    fn something_that_is_not_a_regular_file_is_refused() {
        let logs = Logs::new();
        std::fs::create_dir(logs.logs().join(LOG_FILE)).unwrap();
        assert!(logs.refused().contains("not a regular file"));

        let logs = Logs::new();
        std::fs::remove_dir_all(logs.logs()).unwrap();
        std::fs::write(logs.logs(), b"").unwrap();
        assert!(logs.refused().contains("not a folder"));
    }

    /// Opening a pipe for reading waits for a writer, so one put here would hang
    /// a plain open for ever. This has a time limit so that a failure shows.
    #[test]
    fn a_pipe_at_the_log_is_refused_without_hanging() {
        let logs = Logs::new();
        let made = std::process::Command::new("mkfifo")
            .arg(logs.logs().join(LOG_FILE))
            .status()
            .unwrap();
        assert!(made.success());
        let dirs = logs.dirs.clone();
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = send.send(read_log(&dirs, None, MAX_READ_BYTES).map(|_| ()));
        });
        let answer = receive
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("it hung");
        assert!(answer.unwrap_err().contains("not a regular file"));
    }

    #[test]
    fn a_hard_linked_log_is_refused() {
        let logs = Logs::new();
        logs.write(LOG_FILE, "");
        std::fs::hard_link(logs.logs().join(LOG_FILE), logs.sandbox().join("other")).unwrap();
        assert!(logs.refused().contains("more than one name"));
    }

    #[test]
    fn a_folder_others_can_write_to_makes_the_record_doubtful() {
        let logs = Logs::new();
        logs.write(LOG_FILE, "");
        assert_eq!(logs.read().trust, None);
        std::fs::set_permissions(logs.logs(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(logs.read().trust.unwrap().contains("other accounts"));
    }

    /// Measured: the engine leaves its own log files open to everyone once it has
    /// renamed them. The state folder above is owner-only, so that alone is not a
    /// reason to refuse them or to call them doubtful.
    #[test]
    fn the_file_modes_the_engine_really_uses_are_accepted() {
        let logs = Logs::new();
        logs.write(LOG_FILE, &format!("{}\n", line(&at(1), "a.example.net")));
        std::fs::set_permissions(
            logs.logs().join(LOG_FILE),
            std::fs::Permissions::from_mode(0o666),
        )
        .unwrap();
        let read = logs.read();
        assert_eq!(read.trust, None);
        assert_eq!(read.groups.total, 1);
    }

    #[test]
    fn too_many_names_keep_the_newest_and_say_how_many_were_left() {
        let mut groups = Groups::default();
        for n in 0..(MAX_EVENTS + 5) {
            let time = format!("2026-11-05T07:50:00.{n:06}Z");
            groups.add(line(&time, &format!("n{n}.example.net")).as_bytes());
        }
        let (events, left) = groups.into_events(MAX_EVENTS);
        assert_eq!(left, 5);
        assert_eq!(events.len(), MAX_EVENTS);
        assert_eq!(events.last().unwrap().domain, format!("n{}.example.net", MAX_EVENTS + 4));
    }

    /// The whole reply has to fit in one message, however long each name is.
    #[test]
    fn the_largest_reply_fits_on_the_wire() {
        let row = BlockedEvent {
            time: TIME.to_owned(),
            domain: "\\".repeat(MAX_DOMAIN_BYTES),
            count: u64::MAX,
        };
        let bytes = serde_json::to_vec(&vec![row; MAX_EVENTS]).unwrap();
        assert!(bytes.len() < proto::MAX_FRAME_BYTES, "{}", bytes.len());
    }

    #[test]
    fn only_levels_that_write_these_lines_count_as_recording() {
        assert!(level_records("debug"));
        assert!(level_records("trace"));
        for level in ["info", "warn", "error", "", "DEBUG"] {
            assert!(!level_records(level), "{level}");
        }
    }
}
