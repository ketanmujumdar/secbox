//! `secrets`: values go to the OS keychain, and names to the state folder.
//!
//! Values are never read from arguments and never printed. Any extra argument
//! is caught here rather than by the parser, because the parser's error would
//! echo it, and a stray argument on these commands is likely to be the value.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use secrets::{
    parse_env_file, Assignment, AssignmentSet, AuditAction, BoundHost, Delivery, ProjectId,
    SecretName, SecretStore, ServiceName, StoreError,
};

use crate::state::{
    audit, prepare_project, project_id, read_meta, write_meta, ProjectMeta, SecretSource,
    StatePaths,
};
use crate::ui::{esc, esc_path, not_protecting, paint, Tone};
use crate::{exit, project_dir, say, tell, Deps};

#[derive(Args)]
pub struct SecretsArgs {
    #[command(subcommand)]
    command: SecretsCommand,
}

#[derive(Subcommand)]
enum SecretsCommand {
    /// Import a .env file as data into the keychain. Prints names only
    Import(ImportArgs),
    /// Store one secret, typed at a hidden prompt
    Set(SetArgs),
    /// List secret names and where they go. Never reads the keychain
    List(ListArgs),
    /// Delete one secret from the keychain and the metadata
    Rm(RmArgs),
}

#[derive(Args)]
struct ProjectArg {
    /// Project folder [default: the current folder]
    #[arg(long, value_name = "PATH")]
    project: Option<PathBuf>,
}

#[derive(Args)]
struct ImportArgs {
    /// The file to import
    file: PathBuf,
    /// Show the names it would import, without touching the keychain
    #[arg(long)]
    dry_run: bool,
    #[command(flatten)]
    project: ProjectArg,
}

#[derive(Args)]
struct SetArgs {
    /// Secret name, such as API_KEY
    #[arg(allow_hyphen_values = true)]
    name: String,
    /// Service that may receive it. Repeat for more than one
    #[arg(long = "service", value_name = "NAME")]
    services: Vec<String>,
    /// Keep it on the host and add it in the proxy, for this one host
    #[arg(long, value_name = "HOST", conflicts_with = "build")]
    proxy_host: Option<String>,
    /// Deliver it only as a build secret
    #[arg(long)]
    build: bool,
    #[command(flatten)]
    project: ProjectArg,
    #[arg(hide = true, trailing_var_arg = true, allow_hyphen_values = true)]
    extra: Vec<OsString>,
}

#[derive(Args)]
struct ListArgs {
    #[command(flatten)]
    project: ProjectArg,
}

#[derive(Args)]
struct RmArgs {
    /// Secret name
    #[arg(allow_hyphen_values = true)]
    name: String,
    #[command(flatten)]
    project: ProjectArg,
    #[arg(hide = true, trailing_var_arg = true, allow_hyphen_values = true)]
    extra: Vec<OsString>,
}

pub fn run(args: SecretsArgs, deps: &mut Deps) -> i32 {
    let result = match args.command {
        SecretsCommand::Import(args) => import(&args, deps),
        SecretsCommand::Set(args) => set(&args, deps),
        SecretsCommand::List(args) => list(&args, deps),
        SecretsCommand::Rm(args) => rm(&args, deps),
    };
    match result {
        Ok(code) => code,
        Err(reason) => {
            tell!(deps, "secrets: {reason}");
            exit::FAILED
        }
    }
}

struct Project {
    path: PathBuf,
    id: ProjectId,
    paths: StatePaths,
}

fn open_project(arg: &ProjectArg, deps: &Deps) -> Result<Project, String> {
    let path = project_dir(arg.project.as_deref(), deps)?;
    let id = project_id(&path)?;
    let paths = StatePaths::locate(deps.env).map_err(|e| esc(&e))?;
    Ok(Project { path, id, paths })
}

fn refuse_extra(extra: &[OsString], deps: &mut Deps) -> Option<i32> {
    if extra.is_empty() {
        return None;
    }
    tell!(
        deps,
        "secrets: refused. Values are not accepted on the command line."
    );
    tell!(
        deps,
        "Anything typed there can end up in shell history and process lists, so if"
    );
    tell!(deps, "that was a real secret, replace it with a new one.");
    tell!(
        deps,
        "Run `{} secrets set NAME` and type the value at the hidden prompt.",
        brand::CLI_BIN
    );
    Some(exit::USAGE)
}

fn store_failure(error: StoreError) -> String {
    format!("the keychain refused: {error}")
}

fn load_meta(dir: &Path, project: &Path) -> Result<ProjectMeta, String> {
    Ok(read_meta(dir)?.unwrap_or_else(|| ProjectMeta::new(project)))
}

/// Builds the assignment asked for by the flags, before any secret is touched.
fn requested_assignment(name: &SecretName, args: &SetArgs) -> Result<Option<Assignment>, String> {
    if args.services.is_empty() {
        if args.proxy_host.is_some() || args.build {
            return Err("--proxy-host and --build need at least one --service".to_string());
        }
        return Ok(None);
    }
    let services = args
        .services
        .iter()
        .map(|s| ServiceName::new(s.as_str()).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let delivery = if let Some(host) = &args.proxy_host {
        Delivery::Proxy {
            bound_host: BoundHost::new(host.as_str()).map_err(|e| e.to_string())?,
        }
    } else if args.build {
        Delivery::BuildSecret
    } else {
        Delivery::Injected
    };
    Assignment::new(name.clone(), delivery, services)
        .map(Some)
        .map_err(|e| e.to_string())
}

fn replace_assignment(
    meta: &mut ProjectMeta,
    name: &SecretName,
    new: Option<Assignment>,
) -> Result<(), String> {
    let mut all: Vec<Assignment> = meta
        .assignments
        .iter()
        .filter(|a| a.name() != name)
        .cloned()
        .collect();
    all.extend(new);
    meta.assignments = AssignmentSet::new(all).map_err(|e| e.to_string())?;
    Ok(())
}

fn set(args: &SetArgs, deps: &mut Deps) -> Result<i32, String> {
    if let Some(code) = refuse_extra(&args.extra, deps) {
        return Ok(code);
    }
    let name = SecretName::new(args.name.as_str()).map_err(|e| e.to_string())?;
    let assignment = requested_assignment(&name, args)?;
    let project = open_project(&args.project, deps)?;
    let dir = prepare_project(&project.paths, &project.id)?;
    let mut meta = load_meta(&dir, &project.path)?;

    let label = format!("Value for {name} (hidden): ");
    let no_terminal = |_| "the hidden prompt needs a terminal".to_string();
    let first = (deps.prompt)(&label).map_err(no_terminal)?;
    if first.is_empty() {
        return Err("an empty value was not stored".to_string());
    }
    let second = (deps.prompt)("Type it again to confirm (hidden): ").map_err(no_terminal)?;
    if first.expose() != second.expose() {
        return Err("the two values differ, so nothing was stored".to_string());
    }

    let store = (deps.store)(&project.id);
    store.put(&name, &first).map_err(store_failure)?;
    meta.record(&name, SecretSource::Prompt);
    if assignment.is_some() {
        replace_assignment(&mut meta, &name, assignment)?;
    }
    write_meta(&dir, &meta)?;
    audit(&dir, AuditAction::Stored, &name)?;

    say!(deps, "Stored     {name} in the OS keychain");
    say!(deps, "Project    {}", esc_path(&project.path));
    say!(deps, "Goes to    {}", route(&meta, &name));
    say!(deps, "Nothing delivers secrets yet. {}", not_protecting());
    Ok(exit::OK)
}

fn import(args: &ImportArgs, deps: &mut Deps) -> Result<i32, String> {
    let file = if args.file.is_absolute() {
        args.file.clone()
    } else {
        deps.cwd.join(&args.file)
    };
    let entries = parse_env_file(&file)
        .map_err(|e| format!("cannot import {}: {e}", esc_path(&args.file)))?;
    let project = open_project(&args.project, deps)?;

    if args.dry_run {
        say!(deps, "Dry run. The keychain was not touched.");
        say!(deps, "File       {}", esc_path(&args.file));
        say!(deps, "Project    {}", esc_path(&project.path));
        say!(deps, "Would import {} names:", entries.len());
        for entry in &entries {
            let note = if entry.value.is_empty() {
                "  (empty, would be skipped)"
            } else {
                ""
            };
            say!(deps, "  {}{note}", entry.name);
        }
        return Ok(exit::OK);
    }

    let dir = prepare_project(&project.paths, &project.id)?;
    let mut meta = load_meta(&dir, &project.path)?;
    let store = (deps.store)(&project.id);
    let mut stored = Vec::new();
    let mut outcome = Ok(());
    for entry in &entries {
        if entry.value.is_empty() {
            say!(
                deps,
                "Skipped    {} (empty value, line {})",
                entry.name,
                entry.line
            );
            continue;
        }
        if let Err(error) = store.put(&entry.name, &entry.value) {
            outcome = Err(format!(
                "stopped at {} after {} stored: {}",
                entry.name,
                stored.len(),
                store_failure(error)
            ));
            break;
        }
        let word = if meta.has(&entry.name) {
            "Replaced"
        } else {
            "Imported"
        };
        meta.record(&entry.name, SecretSource::Import);
        stored.push(&entry.name);
        say!(deps, "{word:<11}{}", entry.name);
    }
    // The record comes first: a value in the keychain must never go unrecorded.
    write_meta(&dir, &meta)?;
    for name in &stored {
        audit(&dir, AuditAction::Imported, name)?;
    }
    outcome?;
    say!(deps);
    say!(
        deps,
        "{} stored in the OS keychain for {}",
        stored.len(),
        esc_path(&project.path)
    );
    say!(
        deps,
        "None are assigned to a service, so none would be delivered."
    );
    say!(deps, "Nothing delivers secrets yet. {}", not_protecting());
    Ok(exit::OK)
}

fn route(meta: &ProjectMeta, name: &SecretName) -> String {
    match meta.assignments.iter().find(|a| a.name() == name) {
        None => "no service".to_string(),
        Some(assignment) => {
            let services: Vec<&str> = assignment.services().iter().map(|s| s.as_str()).collect();
            let how = match assignment.delivery() {
                Delivery::Proxy { bound_host } => format!("proxy for {bound_host}"),
                Delivery::Injected => "injected".to_string(),
                Delivery::BuildSecret => "build secret".to_string(),
            };
            format!("{}: {how}", services.join(", "))
        }
    }
}

fn list(args: &ListArgs, deps: &mut Deps) -> Result<i32, String> {
    let project = open_project(&args.project, deps)?;
    say!(deps, "Project    {}", esc_path(&project.path));
    say!(deps, "Id         {}", project.id);
    let meta = read_meta(&project.paths.project(&project.id))?;
    let Some(meta) = meta.filter(|m| !m.secrets.is_empty()) else {
        say!(deps, "No secrets recorded for this project.");
        return Ok(exit::OK);
    };
    say!(deps);
    let width = meta
        .secrets
        .iter()
        .map(|r| r.name.as_str().len())
        .max()
        .unwrap_or(0)
        .max(4);
    let header = format!("{:<width$}  {:<8}  {}", "Name", "Source", "Goes to");
    say!(deps, "{}", paint(deps.color, Tone::Strong, &header));
    for record in &meta.secrets {
        let source = match record.source {
            SecretSource::Prompt => "prompt",
            SecretSource::Import => "import",
        };
        say!(
            deps,
            "{:<width$}  {source:<8}  {}",
            record.name.as_str(),
            route(&meta, &record.name)
        );
    }
    say!(deps);
    say!(
        deps,
        "From the metadata. Values are never shown, and the keychain was not read."
    );
    Ok(exit::OK)
}

fn rm(args: &RmArgs, deps: &mut Deps) -> Result<i32, String> {
    if let Some(code) = refuse_extra(&args.extra, deps) {
        return Ok(code);
    }
    let name = SecretName::new(args.name.as_str()).map_err(|e| e.to_string())?;
    let project = open_project(&args.project, deps)?;
    let dir = prepare_project(&project.paths, &project.id)?;
    let mut meta = load_meta(&dir, &project.path)?;

    let store = (deps.store)(&project.id);
    let in_keychain = delete_value(store.as_ref(), &name)?;
    let in_meta = meta.has(&name);
    if !in_keychain && !in_meta {
        tell!(deps, "secrets: {name} was not found for this project.");
        return Ok(exit::FAILED);
    }
    meta.secrets.retain(|r| r.name != name);
    replace_assignment(&mut meta, &name, None)?;
    write_meta(&dir, &meta)?;
    audit(&dir, AuditAction::Deleted, &name)?;
    let place = if in_keychain {
        "from the OS keychain and the metadata"
    } else {
        "from the metadata; the keychain had no value for it"
    };
    say!(deps, "Removed    {name} {place}");
    Ok(exit::OK)
}

fn delete_value(store: &dyn SecretStore, name: &SecretName) -> Result<bool, String> {
    store.delete(name).map_err(store_failure)
}
