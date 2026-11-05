//! `check`: project detection and Compose checks, as a report.

use std::path::{Path, PathBuf};

use clap::Args;
use projectcheck::compose::{check_project, ComposeReport, Finding, Severity};
use projectcheck::detect::{detect, Detection, ProjectKind};

use crate::ui::{cell, esc, esc_path, not_protecting, paint, Tone};
use crate::{exit, project_dir, say, tell, Deps};

const LABEL: usize = 11;

#[derive(Args)]
pub struct CheckArgs {
    /// Project folder [default: the current folder]
    path: Option<PathBuf>,
    /// Exit with code 3 on Critical or High findings, or an unsupported project
    #[arg(long)]
    strict: bool,
}

pub fn run(args: &CheckArgs, deps: &mut Deps) -> i32 {
    let root = match project_dir(args.path.as_deref(), deps) {
        Ok(root) => root,
        Err(reason) => {
            tell!(deps, "check: {reason}");
            return exit::FAILED;
        }
    };
    let detection = match detect(&root) {
        Ok(detection) => detection,
        Err(error) => {
            tell!(deps, "check: {}", esc(&error.to_string()));
            return exit::FAILED;
        }
    };
    field(deps, "Project", &esc_path(&root));
    let mut blocking = matches!(detection.kind, ProjectKind::Unsupported { .. });
    match describe(&root, &detection, deps) {
        Ok(findings_blocking) => blocking |= findings_blocking,
        Err(reason) => {
            say!(deps);
            tell!(deps, "check: this file could not be checked: {reason}");
            tell!(deps, "Treat the project as unchecked, not as clean.");
            return exit::FAILED;
        }
    }
    if !detection.notes.is_empty() {
        say!(deps);
        say!(deps, "{}", paint(deps.color, Tone::Strong, "Notes"));
        for note in &detection.notes {
            say!(deps, "  {}", esc(note));
        }
    }
    say!(deps);
    say!(deps, "This is a static read of the files. Nothing was run.");
    say!(deps, "{}", paint(deps.color, Tone::Warn, &not_protecting()));
    if args.strict && blocking {
        say!(
            deps,
            "Exiting with code {} because of --strict.",
            exit::FINDINGS
        );
        return exit::FINDINGS;
    }
    exit::OK
}

fn field(deps: &mut Deps, label: &str, value: &str) {
    say!(
        deps,
        "{}{value}",
        cell(deps.color, Tone::Strong, label, LABEL)
    );
}

/// Prints what kind of project this is. Returns true on Critical or High.
fn describe(root: &Path, detection: &Detection, deps: &mut Deps) -> Result<bool, String> {
    match &detection.kind {
        ProjectKind::Compose { file, overrides } => {
            field(deps, "Kind", &format!("Compose, {}", esc_path(file)));
            for name in overrides {
                field(deps, "Override", &esc_path(name));
            }
            let report = check_project(root, file).map_err(|e| esc(&e.to_string()))?;
            print_report(&report, deps);
            Ok(report.findings.iter().any(|f| f.severity >= Severity::High))
        }
        ProjectKind::Dockerfile { file } => {
            field(deps, "Kind", &format!("Dockerfile, {}", esc_path(file)));
            field(deps, "Checks", "none yet: Dockerfile contents are not read");
            Ok(false)
        }
        ProjectKind::DevContainer { file } => {
            field(deps, "Kind", &format!("Dev container, {}", esc_path(file)));
            field(deps, "Checks", "none yet: its contents are not read");
            Ok(false)
        }
        ProjectKind::Manifest {
            file,
            ecosystem,
            suggested_image,
        } => {
            field(
                deps,
                "Kind",
                &format!("{} manifest, {}", esc(ecosystem), esc_path(file)),
            );
            field(
                deps,
                "Base image",
                &format!("{} (suggested)", esc(suggested_image)),
            );
            field(
                deps,
                "Checks",
                "none yet: the manifest contents are not read",
            );
            Ok(false)
        }
        ProjectKind::Unsupported { reason, .. } => {
            let word = paint(deps.color, Tone::Bad, "Refused");
            say!(deps, "{word}    {}", esc(reason));
            Ok(false)
        }
        ProjectKind::Unknown => {
            field(
                deps,
                "Kind",
                "not recognized: no Compose file, Dockerfile or manifest",
            );
            Ok(false)
        }
    }
}

fn print_report(report: &ComposeReport, deps: &mut Deps) {
    let services: Vec<String> = report.model.services.iter().map(|s| esc(&s.name)).collect();
    field(
        deps,
        "Services",
        &format!("{}: {}", services.len(), services.join(", ")),
    );
    let groups = [
        (Severity::Critical, "Critical", Tone::Bad),
        (Severity::High, "High", Tone::Bad),
        (Severity::Medium, "Medium", Tone::Warn),
        (Severity::Info, "Info", Tone::Dim),
    ];
    let counts: Vec<String> = groups
        .iter()
        .map(|(severity, label, _)| {
            let n = report
                .findings
                .iter()
                .filter(|f| f.severity == *severity)
                .count();
            format!("{n} {}", label.to_lowercase())
        })
        .collect();
    field(deps, "Findings", &counts.join(", "));
    for (severity, label, tone) in groups {
        let found: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| f.severity == severity)
            .collect();
        if found.is_empty() {
            continue;
        }
        say!(deps);
        let title = format!("{label} ({})", found.len());
        say!(deps, "{}", paint(deps.color, tone, &title));
        let width = found
            .iter()
            .map(|f| f.service.as_deref().map_or(0, |s| esc(s).chars().count()))
            .max()
            .unwrap_or(0)
            .min(24);
        for finding in found {
            let service = finding.service.as_deref().map_or_else(String::new, esc);
            say!(deps, "  {service:<width$}  {}", esc(&finding.message));
        }
    }
    if !report.model.shared_env.is_empty() {
        say!(deps);
        say!(
            deps,
            "{}",
            paint(
                deps.color,
                Tone::Strong,
                "Environment names shared by services"
            )
        );
        for shared in &report.model.shared_env {
            let services: Vec<String> = shared.services.iter().map(|s| esc(s)).collect();
            say!(deps, "  {}: {}", esc(&shared.key), services.join(", "));
        }
    }
}
