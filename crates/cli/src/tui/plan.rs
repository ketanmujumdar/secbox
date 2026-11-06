//! What a start would do, in the groups the review uses: the sites it could
//! reach, the ports it would open, the secrets it would get and the most it
//! could use, then what the checks found. The start box shows these lines and
//! nothing starts unless the person agrees to them.
//!
//! Every name here came from the project, through the daemon, so each one is
//! escaped again before it is drawn.

use proto::{PlanReport, SecretRoute};

use super::style::Style;
use super::view::Line;
use crate::ui::{esc, human_bytes};

/// The most findings listed. The rest are counted, and `check` lists them all.
const MOST_FINDINGS: usize = 8;

pub fn start_lines(plan: &PlanReport) -> Vec<Line> {
    let from = match &plan.snapshot.revision {
        Some(revision) => format!("commit {}", esc(revision.get(..12).unwrap_or(revision))),
        None => "the working tree, including uncommitted changes".to_owned(),
    };
    let services = plan.services.len();
    let mut lines = vec![Line::plain(format!(
        "{services} service{} from {from}. Nothing else will be allowed.",
        if services == 1 { "" } else { "s" }
    ))];

    lines.push(Line::plain(""));
    lines.push(Line::heading("Reach these sites"));
    lines.push(hosts("while building", &plan.network.build_hosts));
    lines.push(hosts("while running", &plan.network.run_hosts));
    if plan.network.merged_for_whole_life && !plan.network.build_hosts.is_empty() {
        lines.push(Line::styled(
            "  The build hosts stay reachable after the build.",
            Style::WindowWarn,
        ));
    }

    lines.push(Line::plain(""));
    lines.push(Line::heading("Open ports on this computer only"));
    if plan.ports.is_empty() {
        lines.push(Line::dim("  None."));
    } else {
        for port in &plan.ports {
            let guess = if port.dynamic {
                " (a variable can change this)"
            } else {
                ""
            };
            lines.push(Line::plain(format!(
                "  127.0.0.1:{}  {}{guess}",
                port.host_port,
                esc(&port.service)
            )));
        }
        lines.push(Line::dim(
            "  Anything on this machine could then reach them.",
        ));
    }

    lines.push(Line::plain(""));
    lines.push(Line::heading("Get these secrets"));
    if plan.secrets.is_empty() {
        lines.push(Line::dim("  None."));
    } else {
        for secret in &plan.secrets {
            let route = match secret.route {
                SecretRoute::Proxy => "held by the proxy",
                SecretRoute::Injected => "in the environment",
                SecretRoute::InjectedFile => "a file at /run/secrets",
                SecretRoute::BuildSecret => "a build secret",
            };
            let services: Vec<String> = secret.services.iter().map(|s| esc(s)).collect();
            lines.push(Line::plain(format!(
                "  {}  {} ({route})",
                esc(&secret.name),
                services.join(", ")
            )));
        }
    }
    let exposure = proto::exposure_lines(
        &proto::held_from_plan(&plan.secrets),
        Some(&proto::reachable_while_running(&plan.network)),
        true,
    );
    for line in exposure {
        lines.push(Line::styled(format!("  {}", esc(&line)), Style::WindowWarn));
    }

    lines.push(Line::plain(""));
    lines.push(Line::heading("Use up to"));
    let r = &plan.resources;
    lines.push(Line::plain(format!(
        "  {} memory, {} CPUs, {} disk",
        human_bytes(r.memory_bytes),
        f64::from(r.millicpus) / 1000.0,
        human_bytes(r.disk_bytes)
    )));
    if r.build_memory_bytes != r.memory_bytes {
        lines.push(Line::plain(format!(
            "  {} memory while building",
            human_bytes(r.build_memory_bytes)
        )));
    }
    if r.clamped {
        lines.push(Line::dim("  Lowered to your maximum."));
    }

    lines.extend(findings(plan));
    lines
}

/// One line of hosts, wrapped by the box. An empty list is said in words,
/// because a blank would read as unknown.
fn hosts(when: &str, hosts: &[String]) -> Line {
    if hosts.is_empty() {
        return Line::dim(format!("  {when}: nothing"));
    }
    let names: Vec<String> = hosts.iter().map(|h| esc(h)).collect();
    Line::plain(format!("  {when}: {}", names.join(", ")))
}

/// The findings that need a decision, the files that look like secrets, and
/// a count of the rest.
fn findings(plan: &PlanReport) -> Vec<Line> {
    let high: Vec<&proto::Finding> = plan
        .findings
        .iter()
        .filter(|f| f.severity == "Critical" || f.severity == "High")
        .collect();
    let warnings = &plan.snapshot.warnings;
    let count = high.len() + warnings.len();
    let mut lines = vec![
        Line::plain(""),
        Line::heading(match count {
            0 => "Findings".to_owned(),
            1 => "1 finding".to_owned(),
            n => format!("{n} findings"),
        }),
    ];
    if count == 0 {
        lines.push(Line::dim("  Nothing high or critical."));
    }
    for finding in high.iter().take(MOST_FINDINGS) {
        let service = finding
            .service
            .as_deref()
            .map(|s| format!("{}: ", esc(s)))
            .unwrap_or_default();
        lines.push(Line::styled(
            format!(
                "  {}  {service}{}",
                esc(&finding.severity),
                esc(&finding.message)
            ),
            Style::WindowAlert,
        ));
    }
    if high.len() > MOST_FINDINGS {
        lines.push(Line::dim(format!(
            "  and {} more. {} check lists them all.",
            high.len() - MOST_FINDINGS,
            brand::CLI_BIN
        )));
    }
    for path in warnings.iter().take(MOST_FINDINGS) {
        lines.push(Line::styled(
            format!(
                "  {} looks like it holds a secret, and is copied in.",
                esc(path)
            ),
            Style::WindowWarn,
        ));
    }
    if warnings.len() > MOST_FINDINGS {
        lines.push(Line::dim(format!(
            "  and {} more files like it.",
            warnings.len() - MOST_FINDINGS
        )));
    }
    let lower = plan.findings.len() - high.len();
    if lower > 0 {
        lines.push(Line::dim(format!(
            "  {lower} lower finding{}. {} check lists them.",
            if lower == 1 { "" } else { "s" },
            brand::CLI_BIN
        )));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::{Finding, NetworkPlan, PortPlan, ResourcePlan, SecretPlan, SnapshotPlan, TlsPlan};

    fn plan() -> PlanReport {
        PlanReport {
            project_id: "demo-shop-ab12cd34".to_owned(),
            project_path: "/p/demo_shop".to_owned(),
            digest: "9e1d4c07a3b2f611".to_owned(),
            kind: "compose".to_owned(),
            compose_files: vec!["compose.yaml".to_owned()],
            services: vec!["web".to_owned(), "api".to_owned()],
            ports: vec![PortPlan {
                service: "web".to_owned(),
                host_port: 3000,
                container_port: 3000,
                was_all_interfaces: true,
                dynamic: false,
            }],
            resources: ResourcePlan {
                memory_bytes: 6 << 30,
                millicpus: 4000,
                disk_bytes: 30 << 30,
                build_memory_bytes: 8 << 30,
                clamped: false,
            },
            network: NetworkPlan {
                build_hosts: vec!["registry.npmjs.org".to_owned()],
                run_hosts: vec!["api.stripe.com".to_owned()],
                tls: TlsPlan::ServerNameOnly,
                merged_for_whole_life: true,
            },
            secrets: vec![SecretPlan {
                name: "STRIPE_KEY".to_owned(),
                route: SecretRoute::Injected,
                services: vec!["api".to_owned()],
                bound_host: None,
            }],
            snapshot: SnapshotPlan {
                revision: Some("4f2c1a9e0000111122223333".to_owned()),
                counts: None,
                excluded: Vec::new(),
                warnings: vec![".env".to_owned()],
            },
            findings: vec![
                Finding {
                    severity: "High".to_owned(),
                    service: Some("worker".to_owned()),
                    message: "asks for privileged: true".to_owned(),
                },
                Finding {
                    severity: "Low".to_owned(),
                    service: None,
                    message: "no healthcheck".to_owned(),
                },
            ],
            blockers: Vec::new(),
            generated_compose: None,
        }
    }

    fn text(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_plan_is_grouped_as_the_review_groups_it_in_that_order() {
        let lines = start_lines(&plan());
        let headings: Vec<&str> = lines
            .iter()
            .filter(|l| l.style == Style::Title)
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(
            headings,
            [
                "Reach these sites",
                "Open ports on this computer only",
                "Get these secrets",
                "Use up to",
                "2 findings"
            ]
        );
        let all = text(&lines);
        for phrase in [
            "2 services from commit 4f2c1a9e0000.",
            "while building: registry.npmjs.org",
            "while running: api.stripe.com",
            "stay reachable after the build",
            "127.0.0.1:3000  web",
            "STRIPE_KEY  api (in the environment)",
            "6.0 GB memory, 4 CPUs",
            "8.0 GB memory while building",
            "High  worker: asks for privileged: true",
            ".env looks like it holds a secret",
            "1 lower finding",
        ] {
            assert!(all.contains(phrase), "{phrase}: {all}");
        }
    }

    #[test]
    fn nothing_asked_for_is_said_in_words_rather_than_left_blank() {
        let mut quiet = plan();
        quiet.network.build_hosts.clear();
        quiet.network.run_hosts.clear();
        quiet.ports.clear();
        quiet.secrets.clear();
        quiet.findings.clear();
        quiet.snapshot.warnings.clear();
        let all = text(&start_lines(&quiet));
        assert!(all.contains("while building: nothing"), "{all}");
        assert!(all.contains("while running: nothing"), "{all}");
        assert_eq!(all.matches("  None.").count(), 2, "{all}");
        assert!(all.contains("Nothing high or critical."), "{all}");
        assert!(!all.contains("could then reach them"), "{all}");
    }

    /// A hostile name from the project cannot put a control code on screen.
    #[test]
    fn names_from_the_project_are_escaped() {
        let mut hostile = plan();
        hostile.services[0] = "x".to_owned();
        hostile.ports[0].service = "web\u{1b}[2J".to_owned();
        hostile.secrets[0].name = "KEY\u{7}".to_owned();
        hostile.findings[0].message = "line\nbreak".to_owned();
        for line in start_lines(&hostile) {
            assert!(
                line.text.chars().all(|c| !c.is_control()),
                "{:?}",
                line.text
            );
        }
    }

    /// Prints the start box, for a person to look at:
    /// `cargo test -p cli print_start_box -- --ignored --nocapture`.
    #[test]
    #[ignore = "prints screens for a person to read"]
    fn print_start_box() {
        use super::super::view::{compose, Dialog, Model, Row, RowState};
        let mut model = Model {
            projects: vec![Row {
                name: "demo-shop-ab12cd34".to_owned(),
                path: "/p/demo_shop".to_owned(),
                state: RowState::Stopped,
            }],
            ..Model::default()
        };
        model.dialog = Some(Dialog::question(
            "Start demo-shop-ab12cd34",
            start_lines(&plan()),
        ));
        for (w, h) in [(120, 36), (80, 24)] {
            if let Some(dialog) = model.dialog.as_mut() {
                dialog.note_seen(w, h);
            }
            println!("--- start {w}x{h}\n{}", compose(&model, w, h).plain());
        }
    }
}
