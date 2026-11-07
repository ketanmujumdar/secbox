//! Using an env file for a project's secrets: choose the file, see its names,
//! tick who gets them, and the daemon does the rest. Names only reach here:
//! the daemon reads the file and writes the keychain.

use std::ffi::OsString;
use std::time::Duration;

use proto::{
    ProjectArgs, Reply, Request, SecretFileArgs, SecretFilePreview, SecretFileSource,
    UseSecretFileArgs,
};

use super::form::Step;
use super::logs::failure;
use super::style::Style;
use super::view::{Key, Line, Picker, Prompt};
use super::{edit_line, LineEdit, LineKind};
use crate::client::DaemonClient;
use crate::secret_cmd::effect_words;
use crate::ui::esc;

/// How long the daemon may take, sandbox included.
const LIMIT: Duration = Duration::from_secs(30);
/// The names drawn in the box. The rest are counted.
const NAMES_SHOWN: usize = 12;

enum Stage {
    Choose,
    Path,
    Review(Box<SecretFilePreview>),
}

/// What the interface does once the daemon has answered.
pub enum Next {
    Again,
    Done(String),
}

pub struct FileForm {
    stage: Stage,
    files: Vec<SecretFileSource>,
    note: Option<String>,
    cursor: usize,
    path: String,
    services: Vec<(String, bool)>,
    home: Option<OsString>,
}

impl FileForm {
    /// Asks which files there are. A failure still leaves typing a path.
    pub fn find(client: &dyn DaemonClient, project: &str) -> Self {
        let mut form = Self {
            stage: Stage::Choose,
            files: Vec::new(),
            note: None,
            cursor: 0,
            path: String::new(),
            services: Vec::new(),
            home: std::env::var_os("HOME"),
        };
        let answer = client.stream(
            Request::SecretFiles(ProjectArgs { project: project.to_owned() }),
            &mut |_, _| true,
            Some(LIMIT),
        );
        match answer {
            Ok(Reply::SecretFiles(found)) => {
                form.files.extend(found.sandbox.into_iter().map(SecretFileSource::Sandbox));
                form.files.extend(found.host.into_iter().map(SecretFileSource::Host));
                form.note = found.note;
            }
            Ok(_) => form.note = Some("The daemon answered something else.".to_owned()),
            Err(error) => form.note = Some(failure(&error).text),
        }
        form
    }

    pub fn picking(&self) -> bool {
        !matches!(self.stage, Stage::Path)
    }

    /// Text for the path. What follows Enter in the same read is dropped.
    pub fn type_bytes(&mut self, bytes: &[u8]) -> Step {
        match edit_line(&mut self.path, bytes, LineKind::Path) {
            LineEdit::Typing => Step::Stay,
            LineEdit::Cancel => Step::Cancelled,
            LineEdit::Submit if self.path.trim().is_empty() => {
                Step::Stopped("No file was given, so nothing was changed.".to_owned())
            }
            LineEdit::Submit => Step::Send,
        }
    }

    pub fn pick(&mut self, key: Key) -> Step {
        // The choose list ends with a row for typing a path.
        let rows = match &self.stage {
            Stage::Choose => self.files.len() + 1,
            _ => self.services.len(),
        };
        match key {
            Key::Up => self.cursor = self.cursor.saturating_sub(1),
            Key::Down => self.cursor = (self.cursor + 1).min(rows.saturating_sub(1)),
            Key::Space => {
                if matches!(self.stage, Stage::Review(_)) {
                    if let Some(service) = self.services.get_mut(self.cursor) {
                        service.1 = !service.1;
                    }
                }
            }
            Key::Enter if matches!(self.stage, Stage::Choose) && self.cursor == self.files.len() => {
                self.stage = Stage::Path;
            }
            Key::Enter => return Step::Send,
            Key::Back | Key::Quit => return Step::Cancelled,
            _ => {}
        }
        Step::Stay
    }

    /// Takes the step Enter asked for: reads the file, or uses it.
    pub fn proceed(&mut self, client: &dyn DaemonClient, project: &str) -> Next {
        if let Stage::Review(preview) = &self.stage {
            return Next::Done(self.apply(client, project, preview));
        }
        let source = match self.stage {
            Stage::Path => SecretFileSource::Host(self.typed_path()),
            _ => self.files[self.cursor].clone(),
        };
        let answer = client.stream(
            Request::ReadSecretFile(SecretFileArgs { project: project.to_owned(), source }),
            &mut |_, _| true,
            Some(LIMIT),
        );
        match answer {
            Ok(Reply::ReadSecretFile(preview)) if preview.names.is_empty() => {
                Next::Done("That file has no names in it, so nothing was changed.".to_owned())
            }
            Ok(Reply::ReadSecretFile(preview)) => {
                self.services = preview.services.iter().map(|s| (s.clone(), true)).collect();
                self.cursor = 0;
                self.stage = Stage::Review(Box::new(preview));
                Next::Again
            }
            Ok(_) => Next::Done("The daemon answered something else.".to_owned()),
            Err(error) => Next::Done(format!("That file can't be used: {}", failure(&error).text)),
        }
    }

    fn typed_path(&self) -> String {
        let path = self.path.trim();
        match (path.strip_prefix("~/"), &self.home) {
            (Some(rest), Some(home)) => format!("{}/{rest}", home.to_string_lossy()),
            _ => path.to_owned(),
        }
    }

    fn apply(&self, client: &dyn DaemonClient, project: &str, preview: &SecretFilePreview) -> String {
        let answer = client.stream(
            Request::UseSecretFile(UseSecretFileArgs {
                project: project.to_owned(),
                source: preview.source.clone(),
                names: preview.names.iter().map(|n| n.name.clone()).collect(),
                services: self
                    .services
                    .iter()
                    .filter(|(_, on)| *on)
                    .map(|(name, _)| name.clone())
                    .collect(),
            }),
            &mut |_, _| true,
            Some(LIMIT),
        );
        match answer {
            Ok(Reply::UseSecretFile(report)) => report.summary(),
            Ok(_) => "The daemon answered something else.".to_owned(),
            Err(error) => format!("It was not used: {}", failure(&error).text),
        }
    }

    pub fn prompt(&self) -> Option<Prompt> {
        matches!(self.stage, Stage::Path).then(|| Prompt {
            title: "Use a file on this machine".to_owned(),
            lines: vec![Line::plain("The file's full path, such as ~/work/app/.env. It stays where it is.")],
            shown: esc(&self.path),
            submit: "Read",
        })
    }

    pub fn picker(&self) -> Option<Picker> {
        match &self.stage {
            Stage::Path => None,
            Stage::Choose => {
                let mut lines = vec![Line::plain(
                    "Choose the file. Its names show next, and nothing is stored until you agree.",
                )];
                if let Some(note) = &self.note {
                    lines.push(Line::styled(esc(note), Style::WindowDim));
                }
                let mut rows: Vec<String> = self
                    .files
                    .iter()
                    .map(|file| match file {
                        SecretFileSource::Sandbox(path) => format!("sandbox: {}", esc(path)),
                        SecretFileSource::Host(path) => format!("this Mac: {}", esc(path)),
                    })
                    .collect();
                rows.push("Type a path on this machine".to_owned());
                let cursor = self.cursor;
                Some(Picker {
                    title: "Use a file for secrets".to_owned(),
                    lines,
                    rows: rows.into_iter().enumerate().map(|(i, r)| (r, i == cursor)).collect(),
                    cursor,
                })
            }
            Stage::Review(preview) => {
                let mut lines: Vec<Line> = preview
                    .names
                    .iter()
                    .take(NAMES_SHOWN)
                    .map(|n| Line::plain(format!("{}  {}", esc(&n.name), effect_words(n.effect))))
                    .collect();
                if preview.names.len() > NAMES_SHOWN {
                    lines.push(Line::plain(format!("and {} more", preview.names.len() - NAMES_SHOWN)));
                }
                if let SecretFileSource::Sandbox(_) = preview.source {
                    lines.push(Line::styled(
                        "This came from the sandbox, so anything in it could have written these values.",
                        Style::WindowWarn,
                    ));
                }
                if preview.names_only {
                    lines.push(Line::styled("A template, so only its names are used.", Style::WindowDim));
                }
                if let Some(note) = &preview.services_note {
                    lines.push(Line::styled(esc(note), Style::WindowWarn));
                }
                lines.push(Line::plain("Tick who gets them, then press Enter to store them."));
                Some(Picker {
                    title: "Use this file".to_owned(),
                    lines,
                    rows: self.services.iter().map(|(name, on)| (esc(name), *on)).collect(),
                    cursor: self.cursor,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::CallError;
    use proto::{OutputStream, SecretFileEffect, SecretFileName, SecretFilesReport, UseSecretFileReport};
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeDaemon {
        used: RefCell<Option<UseSecretFileArgs>>,
    }

    impl DaemonClient for FakeDaemon {
        fn call(
            &self,
            _request: Request,
            _progress: &mut dyn FnMut(&str, Option<&str>),
        ) -> Result<Reply, CallError> {
            panic!("these calls must have a time limit");
        }

        fn stream(
            &self,
            request: Request,
            _output: &mut dyn FnMut(OutputStream, &str) -> bool,
            limit: Option<Duration>,
        ) -> Result<Reply, CallError> {
            assert!(limit.is_some());
            Ok(match request {
                Request::SecretFiles(_) => Reply::SecretFiles(SecretFilesReport {
                    sandbox: vec![".env.simulation".to_owned()],
                    host: vec![],
                    note: None,
                }),
                Request::ReadSecretFile(args) => Reply::ReadSecretFile(SecretFilePreview {
                    source: args.source,
                    names: vec![SecretFileName { name: "TOKEN".to_owned(), effect: SecretFileEffect::Store }],
                    names_only: false,
                    services: vec!["app".to_owned(), "db".to_owned()],
                    services_note: None,
                }),
                Request::UseSecretFile(args) => {
                    let report = UseSecretFileReport {
                        stored: args.names.clone(),
                        replaced: vec![],
                        empty: vec![],
                        skipped: vec![],
                        services: args.services.clone(),
                        applies_on_next_start: true,
                    };
                    *self.used.borrow_mut() = Some(args);
                    Reply::UseSecretFile(report)
                }
                _ => panic!("wrong request"),
            })
        }
    }

    #[test]
    fn it_chooses_a_file_shows_its_names_and_stores_for_the_services_ticked() {
        let daemon = FakeDaemon::default();
        let mut form = FileForm::find(&daemon, "/p");
        let rows = form.picker().unwrap().rows;
        assert!(rows[0].0.contains(".env.simulation"));
        assert!(matches!(form.pick(Key::Enter), Step::Send));
        assert!(matches!(form.proceed(&daemon, "/p"), Next::Again));

        let shown = format!("{:?}", form.picker().unwrap().lines);
        assert!(shown.contains("TOKEN"));
        assert!(shown.contains("from the sandbox"), "{shown}");

        form.pick(Key::Down);
        form.pick(Key::Space);
        assert!(matches!(form.pick(Key::Enter), Step::Send));
        let Next::Done(said) = form.proceed(&daemon, "/p") else { panic!("not done") };
        assert!(said.contains("Stored 1"), "{said}");

        let used = daemon.used.into_inner().unwrap();
        assert_eq!(used.names, vec!["TOKEN"]);
        assert_eq!(used.services, vec!["app"]);
        assert_eq!(used.source, SecretFileSource::Sandbox(".env.simulation".to_owned()));
    }

    #[test]
    fn the_last_row_types_a_path_and_a_home_folder_is_expanded() {
        let daemon = FakeDaemon::default();
        let mut form = FileForm::find(&daemon, "/p");
        form.home = Some("/home/me".into());
        form.pick(Key::Down);
        assert!(matches!(form.pick(Key::Enter), Step::Stay));
        assert!(form.prompt().is_some());
        form.type_bytes(b"~/app/.env");
        assert_eq!(form.typed_path(), "/home/me/app/.env");
        assert!(matches!(form.type_bytes(b"\r"), Step::Send));
    }

    #[test]
    fn esc_at_any_stage_changes_nothing() {
        let daemon = FakeDaemon::default();
        let mut form = FileForm::find(&daemon, "/p");
        assert!(matches!(form.pick(Key::Back), Step::Cancelled));
        form.pick(Key::Enter);
        form.proceed(&daemon, "/p");
        assert!(matches!(form.pick(Key::Back), Step::Cancelled));
        assert!(daemon.used.into_inner().is_none());
    }
}
