//! A project that has a Dockerfile and no Compose file.
//!
//! Such a project is run as a one-service Compose project that the host writes
//! itself. This module does the two parts of that which read the project:
//!
//! - [`check_dockerfile`] reads the Dockerfile's `EXPOSE` lines, which become
//!   the ports offered for approval. Nothing else in the file is acted on.
//! - [`generate_compose`] writes the Compose file, by serialising a fixed
//!   structure. No text from the project goes into it: the only values are
//!   port numbers and a Dockerfile path that detection fixed in advance.
//!
//! The Dockerfile is hostile input. It is read with a size limit, parsed
//! strictly, and anything that is not a plain TCP port number is reported and
//! left out rather than guessed at. A port left out is merely not offered; a
//! port offered is still forwarded only if a person approves it.

use std::path::Path;

use serde::Serialize;

use crate::compose::{Finding, FindingKind, Severity};
use crate::safe_fs::read_inside;
use crate::{clean, Error};

/// The largest Dockerfile that will be read.
pub const MAX_DOCKERFILE_BYTES: u64 = 1024 * 1024;
/// The most ports offered from one Dockerfile. More than this is not a web
/// app, and a long list would only make the approval harder to read.
pub const MAX_EXPOSED_PORTS: usize = 32;
/// The most `EXPOSE` entries looked at, valid or not.
const MAX_EXPOSE_ENTRIES: usize = 256;
const MAX_FINDINGS: usize = 64;
/// How much of an odd entry is shown back.
const MAX_SHOWN_CHARS: usize = 60;

/// The name of the one service in the generated Compose file.
pub const GENERATED_SERVICE: &str = "app";

/// What the Dockerfile asks for, as far as starting it is concerned.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DockerfileReport {
    /// TCP ports the final build stage exposes, sorted, without repeats.
    pub exposed: Vec<u16>,
    /// Entries that were left out, and why. All text is safe to print.
    pub findings: Vec<Finding>,
}

/// Reads the Dockerfile at `file`, relative to `root`, and reports its ports.
pub fn check_dockerfile(root: &Path, file: &Path) -> Result<DockerfileReport, Error> {
    let text = read_inside(root, file, MAX_DOCKERFILE_BYTES)?;
    Ok(parse_expose(&text))
}

/// Reads the `EXPOSE` instructions in the last build stage.
///
/// What is understood: comments, blank lines, backslash line continuations,
/// heredocs (whose bodies are skipped, so a line inside one is never read as an
/// instruction), and `FROM`, which starts a new stage and drops the ports of
/// the one before, because only the last stage is the image that runs.
///
/// What is not: ports the base image exposes, which are not in this file, and
/// an `escape` parser directive other than the backslash, which makes the
/// whole file be left alone with a finding.
pub fn parse_expose(text: &str) -> DockerfileReport {
    let mut report = Report::default();
    if let Some(escape) = escape_directive(text) {
        if escape != '\\' {
            report.note(&format!(
                "the Dockerfile sets its escape character to `{}`, which this check does not read, so no ports are offered from it",
                clean(&escape.to_string())
            ));
            return report.finish();
        }
    }

    let mut lines = text.lines();
    let mut entries = 0usize;
    while let Some(first) = lines.next() {
        let trimmed = first.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut logical = String::new();
        let mut line = first;
        loop {
            match line.trim_end().strip_suffix('\\') {
                Some(head) => {
                    logical.push_str(head);
                    logical.push(' ');
                }
                None => {
                    logical.push_str(line);
                    break;
                }
            }
            // Comment lines inside a continuation are dropped by Docker too.
            let Some(next) = lines.by_ref().find(|l| !l.trim_start().starts_with('#')) else {
                break;
            };
            line = next;
        }

        for delimiter in heredoc_delimiters(&logical) {
            for body in lines.by_ref() {
                if body.trim_start_matches('\t') == delimiter || body.trim() == delimiter {
                    break;
                }
            }
        }

        let mut words = logical.split_whitespace();
        let Some(instruction) = words.next() else {
            continue;
        };
        if instruction.eq_ignore_ascii_case("FROM") {
            report.exposed.clear();
            continue;
        }
        if !instruction.eq_ignore_ascii_case("EXPOSE") {
            continue;
        }
        for word in words {
            entries += 1;
            if entries > MAX_EXPOSE_ENTRIES {
                report.note(&format!(
                    "the Dockerfile has more than {MAX_EXPOSE_ENTRIES} EXPOSE entries; the rest were not read"
                ));
                return report.finish();
            }
            report.entry(word);
        }
    }
    report.finish()
}

#[derive(Default)]
struct Report {
    exposed: Vec<u16>,
    findings: Vec<Finding>,
    truncated: bool,
    too_many_ports: bool,
}

impl Report {
    fn entry(&mut self, word: &str) {
        let shown = || {
            let cut: String = word.chars().take(MAX_SHOWN_CHARS).collect();
            let more = if word.chars().count() > MAX_SHOWN_CHARS {
                "..."
            } else {
                ""
            };
            format!("`{}{more}`", clean(&cut))
        };
        let (number, protocol) = match word.split_once('/') {
            Some((number, protocol)) => (number, Some(protocol)),
            None => (word, None),
        };
        if word.contains('$') {
            self.note(&format!(
                "EXPOSE {} uses a variable, which is only known at build time, so it is not offered",
                shown()
            ));
            return;
        }
        match protocol {
            None => {}
            Some(p) if p.eq_ignore_ascii_case("tcp") => {}
            Some(p) if p.eq_ignore_ascii_case("udp") => {
                self.note(&format!(
                    "EXPOSE {} is UDP, and only TCP ports can be forwarded, so it is not offered",
                    shown()
                ));
                return;
            }
            Some(_) => {
                self.note(&format!(
                    "EXPOSE {} names a protocol this does not understand, so it is not offered",
                    shown()
                ));
                return;
            }
        }
        if number.contains('-') && number.split('-').all(is_digits) {
            self.note(&format!(
                "EXPOSE {} is a range, and ranges cannot be approved, so it is not offered",
                shown()
            ));
            return;
        }
        let port = if is_digits(number) && number.len() <= 5 {
            number.parse::<u16>().ok().filter(|p| *p != 0)
        } else {
            None
        };
        let Some(port) = port else {
            self.note(&format!(
                "EXPOSE {} is not a port number from 1 to 65535, so it is not offered",
                shown()
            ));
            return;
        };
        if self.exposed.contains(&port) {
            return;
        }
        if self.exposed.len() >= MAX_EXPOSED_PORTS {
            if std::mem::replace(&mut self.too_many_ports, true) {
                return;
            }
            self.note(&format!(
                "the Dockerfile exposes more than {MAX_EXPOSED_PORTS} ports, so port {port} and any after it are not offered"
            ));
            return;
        }
        self.exposed.push(port);
    }

    fn note(&mut self, message: &str) {
        if self.findings.len() >= MAX_FINDINGS {
            self.truncated = true;
            return;
        }
        self.findings.push(Finding {
            severity: Severity::Info,
            kind: FindingKind::DockerfileExpose,
            service: Some(GENERATED_SERVICE.to_owned()),
            message: clean(message),
        });
    }

    fn finish(mut self) -> DockerfileReport {
        if self.truncated {
            self.findings.push(Finding {
                severity: Severity::Info,
                kind: FindingKind::DockerfileExpose,
                service: Some(GENERATED_SERVICE.to_owned()),
                message: format!(
                    "more than {MAX_FINDINGS} notes about EXPOSE: the rest were not listed"
                ),
            });
        }
        self.exposed.sort_unstable();
        DockerfileReport {
            exposed: self.exposed,
            findings: self.findings,
        }
    }
}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// The `escape` parser directive, if the file has one. Directives are only
/// read before the first instruction, comment-like line that is not one, or
/// blank line, which is where Docker reads them.
fn escape_directive(text: &str) -> Option<char> {
    for line in text.lines() {
        // The first line that is not `# key=value` ends the directives.
        let body = line.trim().strip_prefix('#')?;
        let (key, value) = body.split_once('=')?;
        let key = key.trim();
        if key.eq_ignore_ascii_case("escape") {
            let value = value.trim();
            let mut chars = value.chars();
            return match (chars.next(), chars.next()) {
                (Some(c), None) => Some(c),
                // Anything else is not a valid directive; treat it as unknown.
                _ => Some('\0'),
            };
        }
        if key.is_empty() || key.contains(char::is_whitespace) {
            return None;
        }
    }
    None
}

/// The delimiters of heredocs opened on this logical line, in order.
fn heredoc_delimiters(line: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find("<<") {
        rest = &rest[at + 2..];
        let body = rest.strip_prefix('-').unwrap_or(rest);
        let (quoted, body) = match body.chars().next() {
            Some(q @ ('"' | '\'')) => (Some(q), &body[1..]),
            _ => (None, body),
        };
        let word: String = body
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if word.is_empty() {
            continue;
        }
        if let Some(q) = quoted {
            if !body[word.len()..].starts_with(q) {
                continue;
            }
        }
        found.push(word);
        if found.len() >= 16 {
            break;
        }
    }
    found
}

/// The Compose file a Dockerfile project runs as.
#[derive(Serialize)]
struct GeneratedCompose {
    services: GeneratedServices,
}

#[derive(Serialize)]
struct GeneratedServices {
    app: GeneratedService,
}

#[derive(Serialize)]
struct GeneratedService {
    build: GeneratedBuild,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ports: Vec<GeneratedPort>,
}

#[derive(Serialize)]
struct GeneratedBuild {
    context: &'static str,
    dockerfile: String,
}

#[derive(Serialize)]
struct GeneratedPort {
    target: u16,
    /// A string, as Compose's long syntax has it, so a range cannot be meant.
    published: String,
    protocol: &'static str,
}

/// Writes the Compose file for a Dockerfile project: one service named
/// [`GENERATED_SERVICE`], built from `dockerfile` with the project as its
/// context, publishing each port on the same number inside the sandbox. It has
/// no volumes, no privileges and nothing else.
///
/// `dockerfile` must be a plain relative path inside the project, made of
/// ordinary file-name characters. Anything else is refused rather than
/// escaped, because it would have to come from somewhere other than detection.
pub fn generate_compose(dockerfile: &Path, ports: &[u16]) -> Result<String, Error> {
    let refuse = || Error::PathEscapes(dockerfile.to_path_buf());
    let name = dockerfile.to_str().ok_or_else(refuse)?;
    let plain = !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('/')
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
    if !plain {
        return Err(refuse());
    }
    let mut ports: Vec<u16> = ports.iter().copied().filter(|p| *p != 0).collect();
    ports.sort_unstable();
    ports.dedup();
    let compose = GeneratedCompose {
        services: GeneratedServices {
            app: GeneratedService {
                build: GeneratedBuild {
                    context: ".",
                    dockerfile: name.to_owned(),
                },
                ports: ports
                    .into_iter()
                    .map(|port| GeneratedPort {
                        target: port,
                        published: port.to_string(),
                        protocol: "tcp",
                    })
                    .collect(),
            },
        },
    };
    serde_norway::to_string(&compose).map_err(|e| Error::Yaml(clean(&e.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::{parse_str, Interface};

    fn ports(text: &str) -> Vec<u16> {
        parse_expose(text).exposed
    }

    #[test]
    fn plain_ports_are_read_in_any_case_and_sorted() {
        assert_eq!(
            ports("FROM x\nEXPOSE 8080\nexpose 443/tcp 80/TCP\nEXPOSE 80\n"),
            vec![80, 443, 8080]
        );
    }

    #[test]
    fn odd_entries_are_left_out_and_reported() {
        let report = parse_expose(
            "FROM x\nEXPOSE 53/udp $PORT ${P:-1} 8000-8010 70000 0 abc 80/sctp 3000\n",
        );
        assert_eq!(report.exposed, vec![3000]);
        assert_eq!(report.findings.len(), 8, "{:?}", report.findings);
        assert!(report
            .findings
            .iter()
            .all(|f| f.severity == Severity::Info && f.kind == FindingKind::DockerfileExpose));
    }

    #[test]
    fn only_the_last_stage_counts() {
        assert_eq!(
            ports("FROM a AS build\nEXPOSE 9999\nFROM b\nEXPOSE 3000\n"),
            vec![3000]
        );
    }

    #[test]
    fn continuations_and_comments_are_followed() {
        assert_eq!(
            ports("FROM x\n# EXPOSE 1111\nEXPOSE 80 \\\n# a comment\n  443\n"),
            vec![80, 443]
        );
    }

    #[test]
    fn a_heredoc_body_is_not_read_as_instructions() {
        let text = "FROM x\nRUN <<EOF\nEXPOSE 2222\nEOF\nCOPY <<-'END' /a\n\tEXPOSE 3333\n\tEND\nEXPOSE 80\n";
        assert_eq!(ports(text), vec![80]);
    }

    #[test]
    fn another_escape_character_is_not_guessed_at() {
        let report = parse_expose("# escape=`\nFROM x\nEXPOSE 80\n");
        assert!(report.exposed.is_empty());
        assert_eq!(report.findings.len(), 1);
        assert_eq!(ports("# escape=\\\nFROM x\nEXPOSE 80\n"), vec![80]);
    }

    #[test]
    fn hostile_text_is_escaped_and_cut() {
        let long = "x".repeat(10_000);
        let report = parse_expose(&format!("EXPOSE 80\u{1b}[31m {long}\n"));
        for finding in &report.findings {
            assert!(!finding.message.contains('\u{1b}'));
            assert!(finding.message.len() < 400, "{}", finding.message.len());
        }
        assert_eq!(report.findings.len(), 2);
    }

    #[test]
    fn counts_are_capped() {
        let many: Vec<String> = (1000..1100).map(|p| p.to_string()).collect();
        let report = parse_expose(&format!("EXPOSE {}\n", many.join(" ")));
        assert_eq!(report.exposed.len(), MAX_EXPOSED_PORTS);

        let junk = "a ".repeat(5_000);
        let report = parse_expose(&format!("EXPOSE {junk}\n"));
        assert!(report.findings.len() <= MAX_FINDINGS + 2);
    }

    #[test]
    fn the_generated_file_reads_back_as_one_plain_service() {
        let yaml = generate_compose(Path::new("Dockerfile"), &[8080, 80, 80]).unwrap();
        let report = parse_str(&yaml).unwrap();
        assert_eq!(report.model.services.len(), 1);
        let app = &report.model.services[0];
        assert_eq!(app.name, GENERATED_SERVICE);
        assert!(app.has_build);
        assert!(app.mounts.is_empty());
        let published: Vec<(Option<&str>, Option<u16>)> = app
            .ports
            .iter()
            .map(|p| (p.published.as_deref(), p.target))
            .collect();
        assert_eq!(
            published,
            vec![(Some("80"), Some(80)), (Some("8080"), Some(8080))]
        );
        assert!(app.ports.iter().all(|p| p.interface == Interface::All));
        // Nothing but the ports themselves is worth a finding.
        assert!(
            report
                .findings
                .iter()
                .all(|f| f.kind == FindingKind::PortAllInterfaces),
            "{:?}",
            report.findings
        );
    }

    #[test]
    fn no_ports_means_no_ports_section() {
        let yaml = generate_compose(Path::new("Dockerfile"), &[]).unwrap();
        assert!(!yaml.contains("ports"));
        assert!(parse_str(&yaml).unwrap().findings.is_empty());
    }

    #[test]
    fn a_dockerfile_path_that_is_not_plain_is_refused() {
        for bad in [
            "",
            "/etc/x",
            "../Dockerfile",
            "a/../b",
            "a b",
            "a\nb: c",
            "x\"y",
            "./D",
        ] {
            assert!(generate_compose(Path::new(bad), &[]).is_err(), "{bad:?}");
        }
        assert!(generate_compose(Path::new("docker/Dockerfile.dev"), &[]).is_ok());
    }
}
