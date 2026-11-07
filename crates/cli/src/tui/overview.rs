//! What the Overview tab shows about the selected project, worked out from
//! the daemon's status report.
//!
//! Services, their states and their health come from inside the sandbox, so
//! every string is escaped here, before it is drawn, whatever the daemon has
//! already done. The blocked count follows `status`: a count of none is never
//! put as "0 blocked", because that reads as "nothing was blocked", a
//! different and untrue claim.

use proto::{BlockedRecording, BlockedSummary, StatusReport};

use super::style::Style;
use super::view::{Entry, Overview};
use crate::blocked::{trust_note, Weight};
use crate::ui::{esc, human_bytes};

/// The most services listed. A sandbox can claim to run any number.
const MOST_SERVICES: usize = 50;

pub fn build(report: &StatusReport) -> Overview {
    Overview {
        path: esc(&report.project_path),
        problem: None,
        services: report
            .services
            .iter()
            .take(MOST_SERVICES)
            .map(|service| {
                // One word on the right, as the sandbox put it: its health if
                // it reports one, which says more than its state.
                let word = service.health.as_deref().unwrap_or(&service.state);
                let style = match word {
                    "healthy" | "running" => Style::WindowGood,
                    "unhealthy" | "exited" | "dead" => Style::WindowAlert,
                    _ => Style::WindowWarn,
                };
                Entry::new(esc(&service.name), esc(word), style)
            })
            .collect(),
        ports: report
            .ports
            .iter()
            .map(|port| Entry::new(format!("127.0.0.1:{port}"), String::new(), Style::WindowGood))
            .collect(),
        resources: report.resources.as_ref().map_or_else(Vec::new, |r| {
            let mut lines = vec![
                Entry::new("Memory", human_bytes(r.memory_bytes), Style::Window),
                Entry::new(
                    "CPUs",
                    format!("{}", f64::from(r.millicpus) / 1000.0),
                    Style::Window,
                ),
                Entry::new("Disk", human_bytes(r.disk_bytes), Style::Window),
            ];
            if r.build_memory_bytes != r.memory_bytes {
                lines.push(Entry::new(
                    "Building",
                    human_bytes(r.build_memory_bytes),
                    Style::WindowDim,
                ));
            }
            if r.clamped {
                lines.push(Entry::new("Lowered to your maximum", "", Style::WindowDim));
            }
            lines
        }),
        blocked: blocked(report.blocked.as_ref()),
        blocked_alert: blocked_alert(report.blocked.as_ref()),
        blocked_hosts: blocked_hosts(report.blocked.as_ref()),
    }
}

/// How many hostnames were refused, for the Network tab. Only a record that
/// is being kept and holds something gives a number; anything else gives
/// none, never a zero.
fn blocked_hosts(summary: Option<&BlockedSummary>) -> Option<u64> {
    let summary = summary?;
    (summary.problem.is_none()
        && summary.recording == BlockedRecording::Recording
        && summary.total > 0)
        .then_some(summary.domains)
}

/// Whether the Blocked box should catch the eye: something was refused, or
/// nothing is being recorded, or the record itself is in doubt.
fn blocked_alert(summary: Option<&BlockedSummary>) -> bool {
    let Some(summary) = summary else {
        return false;
    };
    summary.problem.is_some()
        || summary.recording == BlockedRecording::NotRecording
        || (summary.recording == BlockedRecording::Recording && summary.total > 0)
        || matches!(trust_note(&summary.trust), Some((Weight::Alert, _)))
}

fn blocked(summary: Option<&BlockedSummary>) -> Vec<Entry> {
    let Some(summary) = summary else {
        return vec![
            Entry::new("Unknown: the daemon is older", "", Style::WindowWarn),
            Entry::new(format!("Restart {} to see this", brand::DAEMON_BIN), "", Style::WindowDim),
        ];
    };
    let cut = if summary.cut_off { ", older cut off" } else { "" };
    let mut lines = match (&summary.problem, summary.recording) {
        (Some(_), _) => vec![
            Entry::new("Unknown: the log could not be read", "", Style::WindowAlert),
            Entry::new("n to see why", "", Style::Window),
        ],
        (None, BlockedRecording::NotRecording) => vec![
            Entry::new("Nothing is being recorded", "", Style::WindowAlert),
            Entry::new("n to see why", "", Style::Window),
        ],
        (None, BlockedRecording::NoSandbox) => {
            vec![Entry::new("Nothing recorded: no sandbox", "", Style::WindowDim)]
        }
        (None, BlockedRecording::Recording) if summary.total == 0 => vec![Entry::new(
            format!("No hostnames recorded so far{cut}"),
            "",
            Style::Window,
        )],
        (None, BlockedRecording::Recording) => vec![
            Entry::new(
                format!("{} refused lookups", summary.total),
                "",
                Style::WindowAlert,
            ),
            Entry::new(
                format!(
                    "of {} hostname{}{cut}",
                    summary.domains,
                    if summary.domains == 1 { "" } else { "s" }
                ),
                "",
                Style::Window,
            ),
            Entry::new("n to see them", "", Style::Window),
        ],
    };
    if let Some((Weight::Alert, _)) = trust_note(&summary.trust) {
        lines.push(Entry::new("The record is not trustworthy", "", Style::WindowAlert));
    }
    lines.push(Entry::new(
        "Hostnames only; not IP or UDP",
        "",
        Style::WindowDim,
    ));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::{LogTrust, ResourcePlan, SandboxState, ServiceState};

    fn report() -> StatusReport {
        StatusReport {
            project_id: "p".to_owned(),
            project_path: "/p/demo_shop".to_owned(),
            sandbox: SandboxState::Running,
            services: vec![
                ServiceState {
                    name: "postgres".to_owned(),
                    state: "running".to_owned(),
                    health: Some("healthy".to_owned()),
                },
                ServiceState {
                    name: "web".to_owned(),
                    state: "restarting".to_owned(),
                    health: None,
                },
            ],
            ports: vec![3000, 8080],
            resources: Some(ResourcePlan {
                memory_bytes: 6 << 30,
                millicpus: 4000,
                disk_bytes: 30 << 30,
                build_memory_bytes: 8 << 30,
                clamped: false,
            }),
            blocked_count: 6,
            blocked: Some(BlockedSummary {
                recording: BlockedRecording::Recording,
                total: 6,
                domains: 3,
                cut_off: false,
                trust: LogTrust::HostOnly,
                problem: None,
            }),
            exposure: None,
            usage: None,
        }
    }

    fn texts(entries: &[Entry]) -> String {
        entries
            .iter()
            .map(|e| format!("{} {}", e.left, e.right))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn services_show_their_health_when_they_have_one_and_their_state_when_not() {
        let o = build(&report());
        assert_eq!(o.services[0], Entry::new("postgres", "healthy", Style::WindowGood));
        assert_eq!(o.services[1], Entry::new("web", "restarting", Style::WindowWarn));
        assert_eq!(texts(&o.ports), "127.0.0.1:3000 \n127.0.0.1:8080 ");
        let resources = texts(&o.resources);
        assert!(resources.contains("Memory 6.0 GB"), "{resources}");
        assert!(resources.contains("CPUs 4"), "{resources}");
        assert!(resources.contains("Building 8.0 GB"), "{resources}");
    }

    #[test]
    fn what_the_sandbox_says_is_escaped() {
        let mut r = report();
        r.services[0].name = "evil\u{1b}[2J".to_owned();
        r.services[0].health = Some("ok\u{202e}".to_owned());
        r.project_path = "/p/\u{7}bell".to_owned();
        let o = build(&r);
        for text in [&o.services[0].left, &o.services[0].right, &o.path] {
            assert!(text.chars().all(|c| !c.is_control() && c != '\u{202e}'), "{text:?}");
        }
    }

    #[test]
    fn a_sandbox_that_claims_endless_services_is_cut_short() {
        let mut r = report();
        r.services = (0..10_000)
            .map(|n| ServiceState {
                name: format!("s{n}"),
                state: "running".to_owned(),
                health: None,
            })
            .collect();
        assert_eq!(build(&r).services.len(), MOST_SERVICES);
    }

    /// The Network tab's count is the number of hostnames, and only a record
    /// being kept that holds something gives one: never a zero.
    #[test]
    fn the_network_tab_counts_hostnames_only_when_something_was_recorded() {
        assert_eq!(build(&report()).blocked_hosts, Some(3));
        let mut none = report();
        none.blocked.as_mut().unwrap().total = 0;
        none.blocked.as_mut().unwrap().domains = 0;
        assert_eq!(build(&none).blocked_hosts, None);
        let mut off = report();
        off.blocked.as_mut().unwrap().recording = BlockedRecording::NotRecording;
        assert_eq!(build(&off).blocked_hosts, None);
        let mut old = report();
        old.blocked = None;
        assert_eq!(build(&old).blocked_hosts, None);
    }

    #[test]
    fn refused_lookups_are_counted_in_alert_colour_with_where_to_see_them() {
        let o = build(&report());
        assert!(o.blocked_alert);
        assert_eq!(o.blocked[0].style, Style::WindowAlert);
        let text = texts(&o.blocked);
        assert!(text.contains("6 refused lookups \nof 3 hostnames"), "{text}");
        assert!(text.contains("n to see them"), "{text}");
        assert!(text.contains("Hostnames only"), "{text}");
    }

    /// None is never put as a count, because "0 blocked" reads as a claim
    /// that nothing was tried.
    #[test]
    fn none_recorded_is_never_shown_as_zero_and_is_not_an_alert() {
        let mut r = report();
        r.blocked = Some(BlockedSummary {
            total: 0,
            domains: 0,
            ..r.blocked.unwrap()
        });
        let o = build(&r);
        assert!(!o.blocked_alert);
        let text = texts(&o.blocked);
        assert!(!text.contains('0'), "{text}");
        assert!(text.contains("No hostnames recorded so far"), "{text}");
    }

    #[test]
    fn a_sandbox_that_records_nothing_or_an_older_daemon_is_said_plainly() {
        let mut r = report();
        r.blocked.as_mut().unwrap().recording = BlockedRecording::NotRecording;
        let o = build(&r);
        assert!(o.blocked_alert);
        assert!(texts(&o.blocked).contains("Nothing is being recorded"));

        r.blocked = None;
        let o = build(&r);
        assert!(!o.blocked_alert);
        assert!(texts(&o.blocked).contains("Unknown"));
    }

    #[test]
    fn a_doubtful_record_raises_the_alert_even_with_nothing_counted() {
        let mut r = report();
        let summary = r.blocked.as_mut().unwrap();
        summary.total = 0;
        summary.trust = LogTrust::Doubtful("moved".to_owned());
        let o = build(&r);
        assert!(o.blocked_alert);
        assert!(texts(&o.blocked).contains("not trustworthy"));
    }
}
