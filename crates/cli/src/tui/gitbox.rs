//! What Git needs from the Changes tab beyond a request: reading a typed
//! command the way the desktop app does, saying what came back, and telling
//! when uncommitted work stopped a pull.

use proto::GitReport;

use super::style::Style;
use super::view::Line;

/// The most lines of Git's output kept for the box, newest last.
const KEPT_OUTPUT: usize = 500;

/// Splits a typed command into its arguments, with quotes and backslashes read
/// as a shell reads them and nothing else: no variable, glob or command is
/// expanded. `None` when a quote is left open.
pub fn split_args(line: &str) -> Option<Vec<String>> {
    let mut args = Vec::new();
    let mut word: Option<String> = None;
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(open) if c == open => quote = None,
            Some('"') if c == '\\' && matches!(chars.peek(), Some('"' | '\\')) => {
                if let (Some(word), Some(next)) = (word.as_mut(), chars.next()) {
                    word.push(next);
                }
            }
            Some(_) => {
                if let Some(word) = word.as_mut() {
                    word.push(c);
                }
            }
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                word.get_or_insert_with(String::new);
            }
            None if c == '\\' && chars.peek().is_some() => {
                if let Some(next) = chars.next() {
                    word.get_or_insert_with(String::new).push(next);
                }
            }
            None if c.is_whitespace() => {
                if let Some(done) = word.take() {
                    args.push(done);
                }
            }
            None => word.get_or_insert_with(String::new).push(c),
        }
    }
    if quote.is_some() {
        return None;
    }
    if let Some(done) = word {
        args.push(done);
    }
    Some(args)
}

/// Whether Git stopped because uncommitted work is in the way, which a stash
/// and a pull can get past. The same test as the desktop app's.
pub fn blocked_by_local_changes(report: &GitReport) -> bool {
    !report.ok
        && report.output.lines().any(|line| {
            line.contains("would be overwritten by merge")
                || line.contains("would be overwritten by checkout")
                || line.contains("Please commit your changes or stash them")
        })
}

/// The command a stuck pull is offered instead: the desktop's.
pub fn autostash_args() -> Vec<String> {
    ["pull", "--no-rebase", "--no-edit", "--autostash"]
        .map(str::to_owned)
        .to_vec()
}

/// What the box says for a report. It came through the daemon, and is escaped
/// again, keeping tabs out of the frame.
pub fn report_lines(report: &GitReport, not_checked: Option<&str>) -> Vec<Line> {
    let clean = |text: &str| crate::ui::esc(&text.replace('\t', "    "));
    let mut lines: Vec<Line> = report.output.lines().map(|line| Line::plain(clean(line))).collect();
    if lines.len() > KEPT_OUTPUT {
        lines.drain(..lines.len() - KEPT_OUTPUT);
    }
    for update in &report.updates {
        lines.push(Line::plain(format!(
            "pushed {} {:.10} -> {:.10}",
            clean(&update.name),
            clean(&update.old),
            clean(&update.new)
        )));
    }
    if let Some(branch) = &report.branch {
        lines.push(Line::plain(format!("on branch {}", clean(branch))));
    }
    if let Some(reason) = &report.refused {
        lines.push(Line::plain(""));
        lines.push(Line::styled(clean(reason), Style::WindowAlert));
    }
    if let Some(said) = not_checked {
        lines.push(Line::plain(""));
        lines.push(Line::plain(said));
    }
    lines
}

/// The box's title, in the words the pull and push boxes have always used.
pub fn report_title(report: &GitReport, word: &str) -> String {
    match (&report.refused, report.ok) {
        (Some(_), _) => format!("{word} refused"),
        (None, true) => format!("{word} done"),
        (None, false) => format!("{word} failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(ok: bool, output: &str) -> GitReport {
        GitReport {
            ok,
            code: Some(i32::from(!ok)),
            output: output.to_owned(),
            branch: Some("main".to_owned()),
            updates: Vec::new(),
            refused: None,
        }
    }

    fn split(line: &str) -> Vec<String> {
        split_args(line).unwrap()
    }

    #[test]
    fn a_typed_command_splits_as_a_shell_would_with_nothing_expanded() {
        assert_eq!(split("log --oneline -10"), ["log", "--oneline", "-10"]);
        assert_eq!(split("  switch   -c  fix "), ["switch", "-c", "fix"]);
        assert_eq!(split(r#"commit -m "two words""#), ["commit", "-m", "two words"]);
        assert_eq!(split("commit -m 'it\\s $HOME'"), ["commit", "-m", "it\\s $HOME"]);
        assert_eq!(split(r#"x "a\"b\\c\d""#), ["x", r#"a"b\c\d"#]);
        assert_eq!(split(r"a\ b"), ["a b"]);
        assert_eq!(split(r#"x """#), ["x", ""]);
        assert_eq!(split("$(touch /tmp/x) `id` ;rm"), ["$(touch", "/tmp/x)", "`id`", ";rm"]);
        assert!(split("   ").is_empty());
    }

    #[test]
    fn an_open_quote_is_not_a_command() {
        assert_eq!(split_args("commit -m 'open"), None);
        assert_eq!(split_args("commit -m \"open"), None);
    }

    #[test]
    fn only_a_failure_from_local_changes_offers_the_stash() {
        for said in [
            "error: Your local changes to the following files would be overwritten by merge:\n  a",
            "error: ... would be overwritten by checkout:",
            "Please commit your changes or stash them before you merge.",
        ] {
            assert!(blocked_by_local_changes(&report(false, said)), "{said}");
            assert!(!blocked_by_local_changes(&report(true, said)), "{said}");
        }
        assert!(!blocked_by_local_changes(&report(false, "fatal: no upstream")));
    }

    #[test]
    fn the_stash_runs_the_desktops_command() {
        assert_eq!(autostash_args(), ["pull", "--no-rebase", "--no-edit", "--autostash"]);
    }

    #[test]
    fn git_s_words_are_escaped_and_a_refusal_stands_out() {
        let mut done = report(false, "a\tb\u{1b}[2J\u{202e}\nnext");
        done.refused = Some("pushing to main\u{1b}]0;t is refused".to_owned());
        let lines = report_lines(&done, Some("nobody was asked"));
        let text: String = lines.iter().map(|l| l.text.clone()).collect::<Vec<_>>().join("\n");
        assert!(!text.contains('\u{1b}') && !text.contains('\u{202e}') && !text.contains('\t'), "{text}");
        assert!(text.contains("on branch main") && text.contains("nobody was asked"), "{text}");
        assert!(lines.iter().any(|l| l.style == Style::WindowAlert && l.text.contains("is refused")));
        assert_eq!(report_title(&done, "Pull"), "Pull refused");
        assert_eq!(report_title(&report(true, ""), "Commit"), "Commit done");
        assert_eq!(report_title(&report(false, ""), "Push"), "Push failed");
    }

    #[test]
    fn a_long_answer_keeps_its_end() {
        let long: String = (0..2000).map(|i| format!("line {i}\n")).collect();
        let lines = report_lines(&report(true, &long), None);
        assert!(lines.iter().any(|l| l.text == "line 1999"));
        assert!(!lines.iter().any(|l| l.text == "line 0"));
    }
}
