//! `uninstall --purge`: removes the state folder, and nothing else.
//!
//! The path has no flag. It is computed from the home folder and the fixed
//! folder name, then checked twice: before listing, and just before removing.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use clap::Args;

use crate::state::{inspect_state, read_meta, same_directory, StateCheck, StatePaths, ENGINE_DIR};
use crate::ui::{esc, esc_path, paint, Tone};
use crate::{exit, say, tell, Deps};

#[derive(Args)]
pub struct UninstallArgs {
    /// Actually remove the state folder. Without this, nothing is removed
    #[arg(long)]
    purge: bool,
    /// Do not ask for the name to be typed
    #[arg(long)]
    yes: bool,
}

pub fn run(args: &UninstallArgs, deps: &mut Deps) -> i32 {
    if !args.purge {
        tell!(
            deps,
            "uninstall: nothing was removed. This command needs --purge."
        );
        tell!(
            deps,
            "To remove the program itself, delete the binary or use your package manager."
        );
        return exit::USAGE;
    }
    match purge(args, deps) {
        Ok(code) => code,
        Err(reason) => {
            tell!(deps, "uninstall: refused, nothing was removed: {reason}");
            exit::FAILED
        }
    }
}

fn purge(args: &UninstallArgs, deps: &mut Deps) -> Result<i32, String> {
    let paths = StatePaths::locate(deps.env).map_err(|e| esc(&e))?;
    let state = &paths.state;
    if state.file_name().is_none_or(|n| n != brand::STATE_DIR) {
        return Err("the state path has an unexpected name".to_string());
    }
    let first = match inspect_state(state).map_err(|e| esc(&e))? {
        StateCheck::Missing => {
            say!(
                deps,
                "Nothing to remove: {} does not exist.",
                esc_path(state)
            );
            return Ok(exit::OK);
        }
        StateCheck::Foreign(reason) => {
            return Err(format!(
                "{} is not safe to remove: {}",
                esc_path(state),
                esc(&reason)
            ))
        }
        StateCheck::Ours(meta) => meta,
    };
    let running = running_sandboxes(state);
    if running > 0 {
        return Err(format!(
            "{running} sandbox sockets in {} are answering, so a VM is running from this folder. Stop it first",
            esc_path(state)
        ));
    }
    let entries = list_entries(state, first.dev())?;

    say!(
        deps,
        "This would remove {} and everything in it:",
        esc_path(state)
    );
    for entry in &entries {
        say!(deps, "  {}", entry);
    }
    say!(deps, "{} items inside the folder.", entries.len());
    say!(deps);
    warn_about_keychain(&paths, deps);

    if !args.yes && !confirmed(deps)? {
        tell!(deps, "Aborted. Nothing was removed.");
        return Ok(exit::FAILED);
    }

    match inspect_state(state).map_err(|e| esc(&e))? {
        StateCheck::Ours(again) if same_directory(&first, &again) => {}
        _ => return Err("the state folder changed during this command".to_string()),
    }
    fs::remove_dir_all(state).map_err(|e| format!("could not remove it: {}", e.kind()))?;
    say!(deps, "Removed {}", esc_path(state));
    say!(
        deps,
        "The program itself is still installed. Remove it with your package manager."
    );
    Ok(exit::OK)
}

/// Counts engine sockets that accept a connection. Removing the folder under a
/// running VM would delete its disk. A stale socket refuses, so it does not count.
fn running_sandboxes(state: &Path) -> usize {
    let agents = state.join(ENGINE_DIR).join("run").join("agent");
    let Ok(entries) = fs::read_dir(agents) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "sock"))
        .filter(|e| UnixStream::connect(e.path()).is_ok())
        .count()
}

fn confirmed(deps: &mut Deps) -> Result<bool, String> {
    let prompt = paint(
        deps.color,
        Tone::Bad,
        &format!("Type {} to remove this folder: ", brand::PRODUCT_NAME),
    );
    let _ = write!(deps.out, "{prompt}");
    let _ = deps.out.flush();
    let mut line = String::new();
    deps.input
        .read_line(&mut line)
        .map_err(|e| format!("could not read the answer: {}", e.kind()))?;
    say!(deps);
    Ok(line.trim_end_matches(['\n', '\r']) == brand::PRODUCT_NAME)
}

/// Walks the folder without following links. Refuses a folder that crosses onto
/// another filesystem, since that means something is mounted inside it.
fn list_entries(state: &Path, device: u64) -> Result<Vec<String>, String> {
    let mut entries = Vec::new();
    let mut pending: Vec<PathBuf> = vec![state.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let listing = fs::read_dir(&dir)
            .map_err(|e| format!("cannot list {}: {}", esc_path(&dir), e.kind()))?;
        for item in listing {
            let item = item.map_err(|e| format!("cannot list a folder: {}", e.kind()))?;
            let path = item.path();
            let meta = fs::symlink_metadata(&path)
                .map_err(|e| format!("cannot inspect {}: {}", esc_path(&path), e.kind()))?;
            if meta.dev() != device {
                return Err(format!("{} is on another filesystem", esc_path(&path)));
            }
            let kind = if meta.file_type().is_symlink() {
                "link, only the link goes"
            } else if meta.is_dir() {
                pending.push(path.clone());
                "folder"
            } else if meta.is_file() {
                "file"
            } else {
                "other"
            };
            entries.push(format!("{}  ({kind})", esc_path(&path)));
        }
    }
    entries.sort();
    Ok(entries)
}

/// Purging removes the record of what is in the keychain, so say what stays.
fn warn_about_keychain(paths: &StatePaths, deps: &mut Deps) {
    let mut names = Vec::new();
    let mut unreadable = 0;
    if let Ok(projects) = fs::read_dir(paths.projects()) {
        for project in projects.flatten() {
            let id = project.file_name().to_string_lossy().into_owned();
            match read_meta(&project.path()) {
                Ok(Some(meta)) => {
                    names.extend(meta.secrets.iter().map(|r| format!("{id}/{}", r.name)));
                }
                Ok(None) => {}
                Err(_) => unreadable += 1,
            }
        }
    }
    names.sort();
    if names.is_empty() && unreadable == 0 {
        say!(deps, "No keychain entries are recorded here.");
        say!(deps);
        return;
    }
    let title = paint(
        deps.color,
        Tone::Warn,
        "This does not touch the OS keychain.",
    );
    say!(
        deps,
        "{title} These entries would stay, and the record of them would go:"
    );
    for name in &names {
        say!(
            deps,
            "  service {}, account {}",
            brand::KEYCHAIN_SERVICE,
            esc(name)
        );
    }
    if unreadable > 0 {
        say!(
            deps,
            "  and {unreadable} projects whose metadata could not be read"
        );
    }
    say!(
        deps,
        "To delete them first, cancel and run `{} secrets rm NAME` in each project.",
        brand::CLI_BIN
    );
    say!(deps);
}
