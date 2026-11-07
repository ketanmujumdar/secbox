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
    /// Import a .env file as data into the keychain. Prints names only.
    /// A template such as .env.example adds its names empty, to fill in later
    Import(ImportArgs),
    /// Type a value for each name that is still empty, at a hidden prompt
    Fill(FillArgs),
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
    /// Add only the names, empty, and drop every value in the file. This is
    /// the default for templates such as .env.example
    #[arg(long)]
    names_only: bool,
    #[command(flatten)]
    project: ProjectArg,
}

#[derive(Args)]
struct FillArgs {
    #[command(flatten)]
    project: ProjectArg,
    #[arg(hide = true, trailing_var_arg = true, allow_hyphen_values = true)]
    extra: Vec<OsString>,
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
    /// Deliver it as a file at /run/secrets/NAME instead of in the environment.
    /// Narrower, but the program has to read a file
    #[arg(long, conflicts_with_all = ["build", "proxy_host"])]
    as_file: bool,
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
        SecretsCommand::Fill(args) => fill(&args, deps),
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
    } else if args.as_file {
        Delivery::InjectedFile
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
    say!(deps, "It is delivered on each start, to the services named. {}", not_protecting());
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

    if args.names_only || is_template(&file) {
        return import_names(args, &entries, &project, deps);
    }

    if args.dry_run {
        say!(deps, "Dry run. The keychain was not touched.");
        say!(deps, "File       {}", esc_path(&args.file));
        say!(deps, "Project    {}", esc_path(&project.path));
        say!(deps, "Would import {} names:", entries.len());
        for entry in &entries {
            let note = if entry.value.is_empty() {
                "  (empty, would be added to fill in later)"
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
    let mut empty = 0;
    let mut outcome = Ok(());
    for entry in &entries {
        if entry.value.is_empty() {
            if meta.add_pending(&entry.name) {
                empty += 1;
                say!(
                    deps,
                    "Empty      {} (line {}), to fill in",
                    entry.name,
                    entry.line
                );
            } else {
                say!(
                    deps,
                    "Kept       {} (empty in the file, line {})",
                    entry.name,
                    entry.line
                );
            }
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
    if empty > 0 {
        say!(
            deps,
            "{empty} empty. Run `{} secrets fill` to type their values.",
            brand::CLI_BIN
        );
    }
    say!(deps, "{}", not_protecting());
    Ok(exit::OK)
}

/// The words that mark a file as a template rather than real values, as in
/// `.env.example`, `example.env` or `.env.local.sample`.
const TEMPLATE_WORDS: &[&str] = &["example", "sample", "template", "dist", "defaults"];

fn is_template(file: &Path) -> bool {
    let Some(name) = file.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.to_ascii_lowercase()
        .split('.')
        .any(|part| TEMPLATE_WORDS.contains(&part))
}

/// Records the file's names as empty. The values are placeholders, or a real
/// key someone pasted by mistake, so they are dropped unread and the keychain
/// is never opened.
fn import_names(
    args: &ImportArgs,
    entries: &[secrets::EnvEntry],
    project: &Project,
    deps: &mut Deps,
) -> Result<i32, String> {
    let existing = read_meta(&project.paths.project(&project.id))?;
    let has = |name: &SecretName| existing.as_ref().is_some_and(|m| m.has(name));
    let waiting = |name: &SecretName| existing.as_ref().is_some_and(|m| m.is_pending(name));

    if args.dry_run {
        say!(
            deps,
            "Dry run. Nothing was recorded and the keychain was not touched."
        );
    }
    say!(deps, "File       {}", esc_path(&args.file));
    say!(deps, "Project    {}", esc_path(&project.path));
    say!(deps, "Names only. The values in this file were not kept.");
    say!(deps);

    let mut meta = match &existing {
        Some(meta) => meta.clone(),
        None => ProjectMeta::new(&project.path),
    };
    let mut added = 0;
    for entry in entries {
        let note = if has(&entry.name) {
            "already stored, left as it is"
        } else if waiting(&entry.name) {
            "already empty, to fill in"
        } else {
            added += 1;
            meta.add_pending(&entry.name);
            if args.dry_run {
                "would be added, empty"
            } else {
                "added, empty"
            }
        };
        say!(deps, "  {:<24} {note}", entry.name.as_str());
    }
    if !args.dry_run && added > 0 {
        let dir = prepare_project(&project.paths, &project.id)?;
        write_meta(&dir, &meta)?;
    }
    say!(deps);
    let empty = meta.pending.len();
    if empty > 0 && !args.dry_run {
        say!(
            deps,
            "{empty} empty. Run `{} secrets fill` to type their values, or",
            brand::CLI_BIN
        );
        say!(deps, "`{} secrets set NAME` for one.", brand::CLI_BIN);
    } else if empty == 0 {
        say!(deps, "Every name already has a value.");
    }
    Ok(exit::OK)
}

fn fill(args: &FillArgs, deps: &mut Deps) -> Result<i32, String> {
    if let Some(code) = refuse_extra(&args.extra, deps) {
        return Ok(code);
    }
    let project = open_project(&args.project, deps)?;
    let dir = prepare_project(&project.paths, &project.id)?;
    let mut meta = load_meta(&dir, &project.path)?;
    if meta.pending.is_empty() {
        say!(deps, "No empty names for this project. Nothing to fill in.");
        return Ok(exit::OK);
    }
    say!(deps, "Project    {}", esc_path(&project.path));
    say!(
        deps,
        "{} empty. Press Enter on its own to skip one.",
        meta.pending.len()
    );
    let store = (deps.store)(&project.id);
    let no_terminal = |_| "the hidden prompt needs a terminal".to_string();
    let (mut stored, mut left) = (0, 0);
    for name in meta.pending.clone() {
        let first = (deps.prompt)(&format!("Value for {name} (hidden): ")).map_err(no_terminal)?;
        if first.is_empty() {
            say!(deps, "Skipped    {name}, still empty");
            left += 1;
            continue;
        }
        let second = (deps.prompt)("Type it again to confirm (hidden): ").map_err(no_terminal)?;
        if first.expose() != second.expose() {
            say!(
                deps,
                "Skipped    {name}: the two values differ, still empty"
            );
            left += 1;
            continue;
        }
        store.put(&name, &first).map_err(store_failure)?;
        meta.record(&name, SecretSource::Prompt);
        // Written after each one: a value in the keychain must never go unrecorded.
        write_meta(&dir, &meta)?;
        audit(&dir, AuditAction::Stored, &name)?;
        stored += 1;
        say!(deps, "Stored     {name} in the OS keychain");
    }
    say!(deps);
    say!(deps, "{stored} stored, {left} still empty.");
    if stored > 0 {
        say!(
            deps,
            "None are assigned to a service yet. Use `{} secrets set NAME --service S`.",
            brand::CLI_BIN
        );
        say!(deps, "{}", not_protecting());
    }
    Ok(exit::OK)
}

fn route(meta: &ProjectMeta, name: &SecretName) -> String {
    match meta.assignments.iter().find(|a| a.name() == name) {
        None => "no service".to_string(),
        Some(assignment) => {
            let services: Vec<&str> = assignment.services().iter().map(|s| s.as_str()).collect();
            let how = match assignment.delivery() {
                Delivery::Proxy { bound_host } => format!("proxy for {bound_host}"),
                Delivery::Injected => "injected into the environment".to_string(),
                Delivery::InjectedFile => "injected as a file".to_string(),
                Delivery::BuildSecret => "build secret".to_string(),
            };
            format!("{}: {how}", services.join(", "))
        }
    }
}

/// One secret as `secrets list` shows it: its name, how it was stored and
/// where it goes. Never its value.
pub(crate) struct Listed {
    pub name: String,
    pub source: &'static str,
    pub goes_to: String,
}

/// A project's secrets, from its metadata in the state folder, in the order
/// they were recorded. This is the whole of what `secrets list` reads, and the
/// full-screen interface reads the same: the keychain is never opened.
pub(crate) fn listing(paths: &StatePaths, id: &ProjectId) -> Result<Vec<Listed>, String> {
    let Some(meta) = read_meta(&paths.project(id))? else {
        return Ok(Vec::new());
    };
    let stored = meta.secrets.iter().map(|record| Listed {
        name: record.name.as_str().to_owned(),
        source: match record.source {
            SecretSource::Prompt => "prompt",
            SecretSource::Import => "import",
        },
        goes_to: route(&meta, &record.name),
    });
    let empty = meta.pending.iter().map(|name| Listed {
        name: name.as_str().to_owned(),
        source: "empty",
        goes_to: "not set yet".to_string(),
    });
    let mut all: Vec<Listed> = stored.chain(empty).collect();
    all.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(all)
}

fn list(args: &ListArgs, deps: &mut Deps) -> Result<i32, String> {
    let project = open_project(&args.project, deps)?;
    say!(deps, "Project    {}", esc_path(&project.path));
    say!(deps, "Id         {}", project.id);
    let secrets = listing(&project.paths, &project.id)?;
    if secrets.is_empty() {
        say!(deps, "No secrets recorded for this project.");
        return Ok(exit::OK);
    }
    say!(deps);
    let width = secrets
        .iter()
        .map(|secret| secret.name.len())
        .max()
        .unwrap_or(0)
        .max(4);
    let header = format!("{:<width$}  {:<8}  {}", "Name", "Source", "Goes to");
    say!(deps, "{}", paint(deps.color, Tone::Strong, &header));
    for secret in &secrets {
        say!(
            deps,
            "{:<width$}  {:<8}  {}",
            secret.name,
            secret.source,
            secret.goes_to
        );
    }
    say!(deps);
    say!(
        deps,
        "From the metadata. Values are never shown, and the keychain was not read."
    );
    if secrets.iter().any(|s| s.source == "empty") {
        say!(
            deps,
            "Empty names have no value. Run `{} secrets fill` to type them.",
            brand::CLI_BIN
        );
    }
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
    let was_empty = meta.is_pending(&name);
    if was_empty && !in_keychain && !in_meta {
        meta.pending.retain(|n| n != &name);
        write_meta(&dir, &meta)?;
        say!(deps, "Removed    {name}, an empty name with no value");
        return Ok(exit::OK);
    }
    meta.pending.retain(|n| n != &name);
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
