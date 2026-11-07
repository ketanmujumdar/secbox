//! Asking the person, not the client, before an action that can send
//! something off this machine.
//!
//! A client's "yes" is a request like any other: the socket only proves the
//! caller runs as this user, and so does every other program they run,
//! including one that a bad dependency or a tricked AI agent started. So for
//! the actions below the daemon asks the person itself, through the system's
//! own prompt ([`personcheck`]), and goes ahead only on their answer:
//!
//! - starting a sandbox, which delivers its secrets (`up`);
//! - letting a sandbox reach one more hostname (`net allow`);
//! - each push through the relay, once its branch changes are known.
//!
//! The prompt names the project and what it will do, so a request someone
//! else made at the same moment does not look like the person's own.
//!
//! Where this build cannot ask anyone (anything but macOS today), the action
//! goes ahead on the client's word, as it did before, and the client and the
//! audit log are both told so.

use std::sync::Mutex;

use proto::{STEP_CONFIRM as ASKING, STEP_UNCONFIRMED as UNCHECKED};

use crate::{audit, refused, ApiError, Progress, Project, Service};

/// One prompt at a time. A second request waits its turn, so two prompts
/// never stack where one could be answered for the other.
static ONE_PROMPT: Mutex<()> = Mutex::new(());

/// What is being confirmed, for the audit log.
#[derive(Debug, Clone, Copy)]
pub enum Act {
    Start,
    AllowHost,
    Push,
}

impl Act {
    fn name(self) -> &'static str {
        match self {
            Act::Start => "start",
            Act::AllowHost => "allow host",
            Act::Push => "push",
        }
    }
}

/// The name a person knows a project by: its folder's name.
pub fn project_name(project: &Project) -> String {
    project
        .root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| project.root.to_string_lossy().into_owned())
}

/// "1 secret", "3 hosts".
pub fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl Service {
    /// Asks the person to confirm `reason`, which completes the sentence
    /// "<the daemon> is trying to …". `Ok` means they did, or that nobody can be
    /// asked here; `Err` means they did not, and nothing should be done.
    pub(crate) fn ask_person(
        &self,
        project: &Project,
        act: Act,
        reason: &str,
        progress: &mut dyn Progress,
    ) -> Result<(), ApiError> {
        let entry = |action| {
            audit::entry(self.deps().now_unix(), action)
                .project(project.id.as_str())
                .subject(act.name())
        };
        let Some(method) = self.deps().person_check() else {
            progress.step(UNCHECKED, None);
            self.record(entry(audit::Action::PersonUnchecked));
            return Ok(());
        };
        let _turn = ONE_PROMPT.lock().unwrap_or_else(|e| e.into_inner());
        if progress.cancelled() {
            return Err(refused("the request was withdrawn before you were asked"));
        }
        progress.step(ASKING, Some(method));
        match self.deps().confirm(reason) {
            personcheck::Outcome::Confirmed => {
                self.record(entry(audit::Action::PersonConfirmed));
                Ok(())
            }
            personcheck::Outcome::Declined(why) => {
                let why = personcheck::clean_reason(&why);
                self.record(entry(audit::Action::PersonDeclined).detail(why.clone()));
                Err(refused(format!("not confirmed with {method}, so nothing was done: {why}")))
            }
        }
    }
}
