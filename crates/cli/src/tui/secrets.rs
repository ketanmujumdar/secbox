//! Reading a project's secret names into the Secrets panel.
//!
//! This reads what `secrets list` reads, through the same function: the
//! names, how each was stored and where it goes, from the metadata in the
//! state folder. The keychain is never opened, so no value can reach the
//! screen, and nothing here asks for a password or a fingerprint.

use std::ffi::OsString;

use secrets::ProjectId;

use super::style::Style;
use super::view::{Line, SecretRow, SecretsPanel};
use crate::state::StatePaths;
use crate::ui::esc;

/// Fills the panel for one project, named by its identity. What it held is
/// dropped first, so a failure cannot leave another project's list showing.
pub fn refresh(env: &dyn Fn(&str) -> Option<OsString>, panel: &mut SecretsPanel, id: &str) {
    panel.rows.clear();
    panel.note = None;
    let listed = ProjectId::new(id.to_owned())
        .map_err(|e| e.to_string())
        .and_then(|id| Ok((StatePaths::locate(env)?, id)))
        .and_then(|(paths, id)| crate::secret_cmd::listing(&paths, &id));
    match listed {
        Ok(listed) if listed.is_empty() => {
            panel.note = Some(Line::plain("No secrets are recorded for this project."));
        }
        Ok(listed) => {
            panel.rows = listed
                .into_iter()
                .map(|secret| SecretRow {
                    name: esc(&secret.name),
                    source: secret.source.to_owned(),
                    goes_to: esc(&secret.goes_to),
                })
                .collect();
        }
        Err(reason) => {
            panel.note = Some(Line::styled(
                format!("The secrets could not be read: {}", esc(&reason)),
                Style::WindowAlert,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{prepare_project, write_meta, ProjectMeta, SecretSource};
    use secrets::{Assignment, AssignmentSet, Delivery, SecretName, ServiceName};

    fn home_var() -> String {
        format!("{}{}", brand::ENV_PREFIX, brand::HOME_ENV_SUFFIX)
    }

    /// A state folder of its own, with one project holding two secrets, one
    /// of them assigned. Only names are written: there is no value anywhere.
    fn state_with_secrets() -> (tempfile::TempDir, String) {
        let home = tempfile::tempdir().unwrap();
        let home_os: OsString = home.path().into();
        let env = |key: &str| (key == home_var()).then(|| home_os.clone());
        let paths = StatePaths::locate(&env).unwrap();
        let id = ProjectId::new("demo-shop-ab12cd34".to_owned()).unwrap();
        let dir = prepare_project(&paths, &id).unwrap();
        let mut meta = ProjectMeta::new(std::path::Path::new("/p/demo_shop"));
        let api_key = SecretName::new("API_KEY").unwrap();
        let token = SecretName::new("DB_PASSWORD").unwrap();
        meta.record(&api_key, SecretSource::Prompt);
        meta.record(&token, SecretSource::Import);
        meta.assignments = AssignmentSet::new(vec![Assignment::new(
            api_key,
            Delivery::Injected,
            vec![ServiceName::new("api").unwrap(), ServiceName::new("worker").unwrap()],
        )
        .unwrap()])
        .unwrap();
        write_meta(&dir, &meta).unwrap();
        (home, id.as_str().to_owned())
    }

    fn filled(home: &std::path::Path, id: &str) -> SecretsPanel {
        let home_os: OsString = home.into();
        let env = |key: &str| (key == home_var()).then(|| home_os.clone());
        let mut panel = SecretsPanel {
            rows: vec![SecretRow {
                name: "LEFT_OVER".to_owned(),
                source: String::new(),
                goes_to: String::new(),
            }],
            ..SecretsPanel::default()
        };
        refresh(&env, &mut panel, id);
        panel
    }

    #[test]
    fn names_and_where_they_go_come_from_the_metadata() {
        let (home, id) = state_with_secrets();
        let panel = filled(home.path(), &id);
        assert_eq!(panel.note, None);
        assert_eq!(panel.rows.len(), 2);
        assert_eq!(panel.rows[0].name, "API_KEY");
        assert_eq!(panel.rows[0].source, "prompt");
        assert!(panel.rows[0].goes_to.contains("api, worker"), "{:?}", panel.rows[0]);
        assert_eq!(panel.rows[1].name, "DB_PASSWORD");
        assert_eq!(panel.rows[1].goes_to, "no service");
    }

    #[test]
    fn a_project_with_none_says_so_and_drops_what_was_shown() {
        let home = tempfile::tempdir().unwrap();
        let panel = filled(home.path(), "notes-ff00ff00");
        assert!(panel.rows.is_empty());
        assert!(panel.note.unwrap().text.contains("No secrets"));
    }

    #[test]
    fn damaged_metadata_is_an_error_not_an_empty_list() {
        let (home, id) = state_with_secrets();
        let home_os: OsString = home.path().into();
        let env = |key: &str| (key == home_var()).then(|| home_os.clone());
        let paths = StatePaths::locate(&env).unwrap();
        let dir = paths.project(&ProjectId::new(id.clone()).unwrap());
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "json") {
                std::fs::write(&path, b"{ not json").unwrap();
            }
        }
        let panel = filled(home.path(), &id);
        assert!(panel.rows.is_empty());
        let note = panel.note.unwrap();
        assert_eq!(note.style, Style::WindowAlert);
    }

    #[test]
    fn a_name_that_is_not_a_project_identity_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let panel = filled(home.path(), "../escape");
        assert!(panel.rows.is_empty());
        assert_eq!(panel.note.unwrap().style, Style::WindowAlert);
    }
}
