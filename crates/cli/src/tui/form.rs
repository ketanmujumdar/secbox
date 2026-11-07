//! Typing a secret into the Secrets tab: its name, its value twice, and the
//! services that get it.
//!
//! The value never reaches the model, because the model is what gets drawn and
//! what `Debug` could print. All the screen is given is one dot per character.
//! Its buffer is made big enough once, so it is never copied by growing, and it
//! is overwritten when this is dropped. As in the desktop app, nothing can show
//! a stored value: this only ever replaces one.

use std::time::Duration;

use proto::{Reply, Request, SecretEntry, SecretText, SetSecretArgs, SetSecretReport, MAX_SECRET_BYTES};

use super::logs::failure;
use super::style::Style;
use super::view::{Key, Line, Picker, Prompt};
use super::{edit_line, LineEdit, LineKind};
use crate::client::DaemonClient;
use crate::ui::esc;

/// How long the daemon may take to store a value.
const STORE_LIMIT: Duration = Duration::from_secs(15);

/// A value being typed. No `Debug` and no `Clone`, so it cannot be printed or
/// left behind in a copy.
struct Hidden(String);

impl Hidden {
    fn new() -> Self {
        // Four spare bytes, so that the last character allowed cannot grow it.
        Self(String::with_capacity(MAX_SECRET_BYTES + 4))
    }
}

/// Overwrites a buffer holding a value. Best effort: nothing here can reach a
/// copy the operating system or the terminal keeps.
pub(super) fn wipe(bytes: &mut [u8]) {
    bytes.fill(0);
    std::hint::black_box(bytes);
}

impl Drop for Hidden {
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        let room = bytes.capacity();
        bytes.resize(room, 0);
        wipe(&mut bytes);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Name,
    Value,
    Again,
    Services,
}

/// What a key did to the form.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Stay,
    /// All of it is in, so it can be sent.
    Send,
    Cancelled,
    /// It stopped, with this to say about why.
    Stopped(String),
}

pub struct SecretForm {
    stage: Stage,
    name: String,
    value: Hidden,
    again: Hidden,
    /// Whether a value is being replaced, which is what the box says.
    stored: bool,
    /// Each service that can be given a secret, and whether this one goes to it.
    services: Vec<(String, bool)>,
    cursor: usize,
    services_note: Option<String>,
}

impl SecretForm {
    /// For a name not on the list yet.
    pub fn add(services: &[String], services_note: Option<String>) -> Self {
        Self::new(Stage::Name, String::new(), false, services, &[], services_note)
    }

    /// For a name already on the list, with the services it goes to now ticked.
    pub fn set(entry: &SecretEntry, services: &[String], services_note: Option<String>) -> Self {
        Self::new(
            Stage::Value,
            entry.name.clone(),
            entry.stored,
            services,
            &entry.services,
            services_note,
        )
    }

    fn new(
        stage: Stage,
        name: String,
        stored: bool,
        services: &[String],
        given: &[String],
        services_note: Option<String>,
    ) -> Self {
        Self {
            stage,
            name,
            value: Hidden::new(),
            again: Hidden::new(),
            stored,
            services: services.iter().map(|s| (s.clone(), given.contains(s))).collect(),
            cursor: 0,
            services_note,
        }
    }

    /// Whether this stage takes one key at a time, rather than typed text.
    pub fn picking(&self) -> bool {
        self.stage == Stage::Services
    }

    /// Typed or pasted text. What follows Enter in the same read is dropped, so
    /// a pasted line cannot also fill in the next stage.
    pub fn type_bytes(&mut self, bytes: &[u8]) -> Step {
        let edit = match self.stage {
            Stage::Name => edit_line(&mut self.name, bytes, LineKind::Name),
            Stage::Value => edit_line(&mut self.value.0, bytes, LineKind::Secret),
            Stage::Again => edit_line(&mut self.again.0, bytes, LineKind::Secret),
            Stage::Services => return Step::Stay,
        };
        match edit {
            LineEdit::Typing => Step::Stay,
            LineEdit::Cancel => Step::Cancelled,
            LineEdit::Submit => self.next_stage(),
        }
    }

    fn next_stage(&mut self) -> Step {
        match self.stage {
            Stage::Name => match secrets::SecretName::new(self.name.trim()) {
                Ok(_) => {
                    self.name = self.name.trim().to_owned();
                    self.stage = Stage::Value;
                    Step::Stay
                }
                Err(_) => Step::Stopped(
                    "That is not a valid secret name. Use letters, digits and underscores, starting with a letter or underscore.".to_owned(),
                ),
            },
            Stage::Value if self.value.0.is_empty() => {
                Step::Stopped("An empty value was not stored.".to_owned())
            }
            Stage::Value => {
                self.stage = Stage::Again;
                Step::Stay
            }
            Stage::Again if self.value.0 != self.again.0 => {
                Step::Stopped("The two values differ, so nothing was stored.".to_owned())
            }
            Stage::Again => {
                self.stage = Stage::Services;
                Step::Stay
            }
            Stage::Services => Step::Send,
        }
    }

    /// A key on the list of services.
    pub fn pick(&mut self, key: Key) -> Step {
        let last = self.services.len().saturating_sub(1);
        match key {
            Key::Up => self.cursor = self.cursor.saturating_sub(1),
            Key::Down => self.cursor = (self.cursor + 1).min(last),
            Key::Space => {
                if let Some(service) = self.services.get_mut(self.cursor) {
                    service.1 = !service.1;
                }
            }
            Key::Enter => return Step::Send,
            Key::Back | Key::Quit => return Step::Cancelled,
            _ => {}
        }
        Step::Stay
    }

    /// The line to draw, for a stage that types. A value is only ever dots.
    pub fn prompt(&self) -> Option<Prompt> {
        let verb = if self.stored { "Replace" } else { "Set" };
        let name = esc(&self.name);
        let (title, lines, shown, submit) = match self.stage {
            Stage::Name => (
                "Add a secret".to_owned(),
                vec![Line::plain("Letters, digits and underscores, starting with a letter or underscore.")],
                esc(&self.name),
                "Next",
            ),
            Stage::Value => (
                format!("{verb} {name}"),
                vec![
                    Line::plain("Type the value. It goes to your keychain, and nothing here can show it again."),
                    Line::styled("A running sandbox gets it on its next start.", Style::WindowDim),
                ],
                dots(&self.value.0),
                "Next",
            ),
            Stage::Again => (
                format!("{verb} {name}"),
                vec![Line::plain("Type it again, to be sure of what you typed.")],
                dots(&self.again.0),
                "Next",
            ),
            Stage::Services => return None,
        };
        Some(Prompt { title, lines, shown, submit })
    }

    /// The list to draw, for the stage that ticks services.
    pub fn picker(&self) -> Option<Picker> {
        if self.stage != Stage::Services {
            return None;
        }
        let mut lines = vec![Line::plain(
            "Tick each service that may read it. A service can read every secret it is given. Tick none to store it and give it to nothing yet.",
        )];
        if let Some(note) = &self.services_note {
            lines.push(Line::styled(esc(note), Style::WindowWarn));
        }
        Some(Picker {
            title: format!("Who gets {}", esc(&self.name)),
            lines,
            rows: self.services.iter().map(|(name, on)| (esc(name), *on)).collect(),
            cursor: self.cursor,
        })
    }

    /// Sends it, and says what happened. The value goes with the request, whose
    /// buffer is wiped when the request is dropped.
    pub fn send(mut self, client: &dyn DaemonClient, project: &str) -> String {
        let services = self
            .services
            .iter()
            .filter(|(_, on)| *on)
            .map(|(name, _)| name.clone())
            .collect();
        let value = std::mem::take(&mut self.value.0);
        let answer = client.stream(
            Request::SetSecret(SetSecretArgs {
                project: project.to_owned(),
                name: self.name.clone(),
                value: SecretText::new(value),
                services,
            }),
            &mut |_, _| true,
            Some(STORE_LIMIT),
        );
        match answer {
            Ok(Reply::SetSecret(report)) => stored_message(&report),
            Ok(_) => "The daemon answered something else.".to_owned(),
            Err(error) => format!("It was not stored: {}", failure(&error).text),
        }
    }
}

fn dots(value: &str) -> String {
    "•".repeat(value.chars().count())
}

/// The desktop app's wording for the same answer.
pub fn stored_message(report: &SetSecretReport) -> String {
    let name = esc(&report.name);
    let verb = if report.replaced { "Replaced" } else { "Stored" };
    let goes = if report.services.is_empty() {
        "It goes to no service yet.".to_owned()
    } else {
        let services: Vec<String> = report.services.iter().map(|s| esc(s)).collect();
        format!("It goes to {}.", services.join(", "))
    };
    let when = if report.applies_on_next_start {
        " The running sandbox gets it on its next start."
    } else {
        ""
    };
    format!("{verb} {name} in your keychain. {goes}{when}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::CallError;
    use proto::OutputStream;
    use std::cell::RefCell;

    const VALUE: &str = "correct-horse-battery";

    fn services() -> Vec<String> {
        vec!["api".to_owned(), "worker".to_owned()]
    }

    fn entry() -> SecretEntry {
        SecretEntry {
            name: "API_KEY".to_owned(),
            stored: true,
            services: vec!["worker".to_owned()],
            route: None,
        }
    }

    fn typed(form: &mut SecretForm, text: &str) -> Step {
        form.type_bytes(format!("{text}\r").as_bytes())
    }

    #[test]
    fn a_new_secret_asks_its_name_then_its_value_twice_then_its_services() {
        let mut form = SecretForm::add(&services(), None);
        assert_eq!(form.prompt().unwrap().title, "Add a secret");
        assert_eq!(typed(&mut form, "API_KEY"), Step::Stay);
        assert!(form.prompt().unwrap().title.contains("API_KEY"));
        assert_eq!(typed(&mut form, VALUE), Step::Stay);
        assert_eq!(typed(&mut form, VALUE), Step::Stay);
        assert!(form.picking());
        assert!(form.prompt().is_none());
        let picker = form.picker().unwrap();
        assert_eq!(picker.rows, vec![("api".to_owned(), false), ("worker".to_owned(), false)]);
        form.pick(Key::Down);
        form.pick(Key::Space);
        assert!(form.picker().unwrap().rows[1].1);
        assert_eq!(form.pick(Key::Enter), Step::Send);
    }

    #[test]
    fn replacing_starts_at_the_value_with_its_services_ticked() {
        let form = SecretForm::set(&entry(), &services(), None);
        assert!(form.prompt().unwrap().title.starts_with("Replace API_KEY"));
        assert!(!form.picking());
        let mut form = form;
        typed(&mut form, VALUE);
        typed(&mut form, VALUE);
        assert_eq!(form.picker().unwrap().rows[1], ("worker".to_owned(), true));
    }

    /// Everything that can reach the screen or a debug print is only dots.
    #[test]
    fn a_value_is_never_in_what_is_drawn() {
        let mut form = SecretForm::set(&entry(), &services(), None);
        form.type_bytes(VALUE.as_bytes());
        let prompt = form.prompt().unwrap();
        assert_eq!(prompt.shown, "•".repeat(VALUE.chars().count()));
        assert!(!format!("{prompt:?}").contains(VALUE));
        typed(&mut form, "");
        form.type_bytes(VALUE.as_bytes());
        assert!(!format!("{:?}", form.prompt().unwrap()).contains(VALUE));
        typed(&mut form, "");
        assert!(!format!("{:?}", form.picker().unwrap()).contains(VALUE));
    }

    #[test]
    fn what_follows_enter_in_a_paste_does_not_fill_in_the_next_stage() {
        let mut form = SecretForm::set(&entry(), &services(), None);
        form.type_bytes(format!("{VALUE}\r{VALUE}\r").as_bytes());
        assert!(!form.picking());
        assert_eq!(form.prompt().unwrap().shown, "");
    }

    #[test]
    fn two_values_that_differ_and_an_empty_one_store_nothing() {
        let mut form = SecretForm::set(&entry(), &services(), None);
        typed(&mut form, "one");
        assert_eq!(
            typed(&mut form, "two"),
            Step::Stopped("The two values differ, so nothing was stored.".to_owned())
        );
        let mut form = SecretForm::set(&entry(), &services(), None);
        assert!(matches!(typed(&mut form, ""), Step::Stopped(_)));
    }

    #[test]
    fn a_bad_name_stops_and_esc_cancels_at_every_stage() {
        let mut form = SecretForm::add(&services(), None);
        assert!(matches!(typed(&mut form, "1 bad"), Step::Stopped(_)));
        let mut form = SecretForm::set(&entry(), &services(), None);
        assert_eq!(form.type_bytes(b"\x1b"), Step::Cancelled);
        let mut form = SecretForm::set(&entry(), &services(), None);
        typed(&mut form, VALUE);
        typed(&mut form, VALUE);
        assert_eq!(form.pick(Key::Back), Step::Cancelled);
    }

    #[test]
    fn a_value_buffer_never_grows_so_it_is_never_copied_elsewhere() {
        let mut form = SecretForm::set(&entry(), &services(), None);
        let start = form.value.0.as_ptr();
        form.type_bytes(&vec![b'x'; MAX_SECRET_BYTES + 100]);
        assert_eq!(form.value.0.as_ptr(), start);
        assert_eq!(form.value.0.len(), MAX_SECRET_BYTES);
    }

    /// The project, the name, the services, and whether a limit was given.
    type Seen = (String, String, Vec<String>, bool);

    struct FakeDaemon {
        seen: RefCell<Vec<Seen>>,
    }

    impl DaemonClient for FakeDaemon {
        fn call(
            &self,
            _request: Request,
            _progress: &mut dyn FnMut(&str, Option<&str>),
        ) -> Result<Reply, CallError> {
            panic!("storing a value must use a call with a time limit");
        }

        fn stream(
            &self,
            request: Request,
            _output: &mut dyn FnMut(OutputStream, &str) -> bool,
            limit: Option<Duration>,
        ) -> Result<Reply, CallError> {
            let Request::SetSecret(args) = request else {
                panic!("wrong request");
            };
            self.seen.borrow_mut().push((
                args.project.clone(),
                args.name.clone(),
                args.services.clone(),
                limit.is_some(),
            ));
            assert_eq!(args.value.expose(), VALUE);
            Ok(Reply::SetSecret(SetSecretReport {
                name: args.name.clone(),
                replaced: true,
                services: args.services,
                applies_on_next_start: true,
            }))
        }
    }

    #[test]
    fn it_sends_the_same_request_as_the_desktop_and_says_what_it_did() {
        let mut form = SecretForm::set(&entry(), &services(), None);
        typed(&mut form, VALUE);
        typed(&mut form, VALUE);
        let daemon = FakeDaemon { seen: RefCell::new(Vec::new()) };
        let said = form.send(&daemon, "/p/crm");
        assert_eq!(
            daemon.seen.into_inner(),
            vec![("/p/crm".to_owned(), "API_KEY".to_owned(), vec!["worker".to_owned()], true)]
        );
        assert_eq!(
            said,
            "Replaced API_KEY in your keychain. It goes to worker. The running sandbox gets it on its next start."
        );
        assert!(!said.contains(VALUE));
    }
}
