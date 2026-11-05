//! Which editors are known, and where each keeps its user settings.
//!
//! Paths are built from the home folder and a fixed app folder name, never
//! from anything a project can write. The platform is a parameter so both
//! layouts can be tested on either machine.

use std::ffi::OsString;
use std::path::{Component, PathBuf};

/// The editors covered. All three are builds of the same editor, so they share
/// the setting names and differ only in their folder and command names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Editor {
    Code,
    CodeInsiders,
    Cursor,
}

pub const EDITORS: [Editor; 3] = [Editor::Code, Editor::CodeInsiders, Editor::Cursor];

impl Editor {
    /// The name used in check ids and on the command line.
    pub fn id(self) -> &'static str {
        match self {
            Editor::Code => "vscode",
            Editor::CodeInsiders => "vscode-insiders",
            Editor::Cursor => "cursor",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Editor::Code => "VS Code",
            Editor::CodeInsiders => "VS Code Insiders",
            Editor::Cursor => "Cursor",
        }
    }

    /// The folder the editor keeps its user data in, under the per-platform
    /// application folder.
    fn app_dir(self) -> &'static str {
        match self {
            Editor::Code => "Code",
            Editor::CodeInsiders => "Code - Insiders",
            Editor::Cursor => "Cursor",
        }
    }

    /// The command the editor installs on the PATH.
    pub fn command(self) -> &'static str {
        match self {
            Editor::Code => "code",
            Editor::CodeInsiders => "code-insiders",
            Editor::Cursor => "cursor",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        EDITORS.into_iter().find(|e| e.id() == id)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Platform {
    Mac,
    Linux,
}

impl Platform {
    /// The platform this binary was built for. Windows is not supported.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Platform::Mac
        } else {
            Platform::Linux
        }
    }
}

/// Where one editor's user data lives, and the settings file inside it.
#[derive(Clone, Debug)]
pub struct UserSettings {
    pub user_dir: PathBuf,
    pub settings: PathBuf,
}

/// Documented at <https://code.visualstudio.com/docs/configure/settings>:
/// macOS `$HOME/Library/Application Support/Code/User/settings.json`, Linux
/// `$HOME/.config/Code/User/settings.json`. Other builds of the same editor
/// swap the folder name. On Linux the editor honors `XDG_CONFIG_HOME`.
pub fn user_settings(
    home: &std::path::Path,
    xdg_config: Option<&std::path::Path>,
    platform: Platform,
    editor: Editor,
) -> UserSettings {
    let base = match platform {
        Platform::Mac => home.join("Library").join("Application Support"),
        Platform::Linux => match xdg_config {
            Some(dir) if dir.is_absolute() => dir.to_path_buf(),
            _ => home.join(".config"),
        },
    };
    let user_dir = base.join(editor.app_dir()).join("User");
    let settings = user_dir.join("settings.json");
    UserSettings { user_dir, settings }
}

/// The home folder, taken the same way the state folder takes it: the override
/// variable first, then `HOME`. It must be absolute and free of dot segments,
/// so no setting can point it at a relative or climbing path.
pub fn home(env: &dyn Fn(&str) -> Option<OsString>) -> Result<PathBuf, String> {
    let var = crate::state::home_override_var();
    let (value, source) = match env(&var) {
        Some(value) => (value, var),
        None => match env("HOME") {
            Some(value) => (value, "HOME".to_string()),
            None => return Err("HOME is not set".to_string()),
        },
    };
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(format!("{source} must be an absolute path"));
    }
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(format!("{source} must not contain . or .. segments"));
    }
    Ok(path)
}

/// `XDG_CONFIG_HOME`, used only when it is an absolute path.
pub fn xdg_config(env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let path = PathBuf::from(env("XDG_CONFIG_HOME")?);
    path.is_absolute().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn settings_paths_match_the_documented_layout() {
        let home = Path::new("/home/u");
        let mac = user_settings(home, None, Platform::Mac, Editor::Code);
        assert_eq!(
            mac.settings,
            Path::new("/home/u/Library/Application Support/Code/User/settings.json")
        );
        let linux = user_settings(home, None, Platform::Linux, Editor::Cursor);
        assert_eq!(
            linux.settings,
            Path::new("/home/u/.config/Cursor/User/settings.json")
        );
        let insiders = user_settings(home, None, Platform::Mac, Editor::CodeInsiders);
        assert!(insiders
            .settings
            .starts_with("/home/u/Library/Application Support/Code - Insiders"));
    }

    #[test]
    fn xdg_moves_the_linux_path_but_only_when_absolute() {
        let home = Path::new("/home/u");
        let moved = user_settings(home, Some(Path::new("/cfg")), Platform::Linux, Editor::Code);
        assert_eq!(moved.settings, Path::new("/cfg/Code/User/settings.json"));
        let relative = user_settings(home, Some(Path::new("cfg")), Platform::Linux, Editor::Code);
        assert_eq!(
            relative.settings,
            Path::new("/home/u/.config/Code/User/settings.json")
        );
        // The application folder on macOS never moves with XDG_CONFIG_HOME.
        let mac = user_settings(home, Some(Path::new("/cfg")), Platform::Mac, Editor::Code);
        assert!(mac.settings.starts_with("/home/u/Library"));
    }

    #[test]
    fn home_refuses_relative_and_climbing_paths() {
        let bad = |value: &'static str| {
            let env = move |key: &str| (key == "HOME").then(|| OsString::from(value));
            home(&env)
        };
        assert!(bad("relative").is_err());
        assert!(bad("/home/../etc").is_err());
        assert!(bad("").is_err());
        assert_eq!(bad("/home/u").unwrap(), Path::new("/home/u"));
        let none = |_: &str| None;
        assert!(home(&none).is_err());
    }

    #[test]
    fn editor_ids_round_trip() {
        for editor in EDITORS {
            assert_eq!(Editor::from_id(editor.id()), Some(editor));
        }
        assert_eq!(Editor::from_id("emacs"), None);
    }
}
