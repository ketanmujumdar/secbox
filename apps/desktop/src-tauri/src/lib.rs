//! The desktop app: a menu bar icon that is always there, and a window.
//!
//! Both are clients of the daemon with no powers of their own. The window is
//! a static page served by Tauri's own asset protocol, so there is no local
//! web port, and it can reach only the commands in this file, each granted by
//! name in `capabilities/main.json`.

pub mod client;
pub mod daemon_proc;
pub mod ops;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent, State, WindowEvent};

use client::{CallError, Client, Stopper};
use ops::{BlockedView, DiffView, PlanView, ProjectRow, ShownPlans, StatusView};

const TRAY_ID: &str = "main";
const WINDOW: &str = "main";
const TRAY_REFRESH: Duration = Duration::from_secs(10);
const TRAY_PROJECTS: usize = 10;

/// Everything the commands share.
pub struct AppState {
    client: Client,
    paths: Result<hoststate::StatePaths, String>,
    exe: Option<PathBuf>,
    daemon_log: PathBuf,
    /// What the app did about the daemon at launch, for the window to say.
    notice: Mutex<Option<String>>,
    plans: ShownPlans,
    logs: Mutex<HashMap<u64, Stopper>>,
    next_log: AtomicU64,
    /// Only one start or restart of the daemon at a time.
    daemon_lock: Mutex<()>,
}

impl AppState {
    fn new(daemon_log: PathBuf) -> Self {
        let env = |key: &str| std::env::var_os(key);
        let paths = ops::locate_state(&env);
        let client = match &paths {
            Ok(paths) => Client::new(&paths.state),
            // A socket that cannot exist, so every call says the daemon is
            // not running, next to the reason the window shows.
            Err(_) => Client::new(std::path::Path::new("/nonexistent")),
        };
        Self {
            client,
            paths,
            exe: std::env::current_exe().ok(),
            daemon_log,
            notice: Mutex::new(None),
            plans: ShownPlans::default(),
            logs: Mutex::new(HashMap::new()),
            next_log: AtomicU64::new(1),
            daemon_lock: Mutex::new(()),
        }
    }

    fn daemon_binary(&self) -> Option<PathBuf> {
        let env = |key: &str| std::env::var_os(key);
        daemon_proc::find_daemon(self.exe.as_deref(), &env)
    }

    fn set_notice(&self, text: String) {
        *self.notice.lock().unwrap_or_else(|e| e.into_inner()) = Some(text);
    }

    /// Starts the daemon when nothing answers, or restarts it.
    fn start_daemon(&self, restart: bool) -> Result<String, String> {
        let _one = self.daemon_lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(reason) = &self.paths {
            return Err(format!("Cannot find the state folder: {reason}"));
        }
        if !restart && self.client.ping().is_ok() {
            return Ok("The daemon is already running.".to_owned());
        }
        let Some(binary) = self.daemon_binary() else {
            return Err(format!(
                "{} was not found beside this app or on PATH, so it cannot be started from here.",
                brand::DAEMON_BIN
            ));
        };
        let shown = proto::text::escape_path(&binary);
        if restart {
            daemon_proc::restart_daemon(&binary, &self.daemon_log, &self.client)?;
            Ok(format!("Restarted {} from {shown}.", brand::DAEMON_BIN))
        } else {
            daemon_proc::spawn_daemon(&binary, &self.daemon_log, &self.client)?;
            Ok(format!(
                "No daemon was answering, so this app started {} from {shown}. It keeps running after you quit the app.",
                brand::DAEMON_BIN
            ))
        }
    }

    fn paths(&self) -> Result<&hoststate::StatePaths, String> {
        self.paths
            .as_ref()
            .map_err(|reason| format!("Cannot find the state folder: {reason}"))
    }
}

fn message(error: CallError) -> String {
    error.message()
}

/// Runs blocking work off the window's thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|_| "The app failed while doing that.".to_owned())?
}

#[derive(Serialize)]
struct AppInfo {
    product: &'static str,
    cli_bin: &'static str,
    daemon_bin: &'static str,
    version: &'static str,
    state_dir: Option<String>,
    /// The command line tool shipped inside the app, if this build has one.
    bundled_cli: Option<String>,
}

#[tauri::command]
fn app_info(state: State<'_, Arc<AppState>>) -> AppInfo {
    AppInfo {
        product: brand::PRODUCT_NAME,
        cli_bin: brand::CLI_BIN,
        daemon_bin: brand::DAEMON_BIN,
        version: env!("CARGO_PKG_VERSION"),
        state_dir: state
            .paths
            .as_ref()
            .ok()
            .map(|p| proto::text::escape_path(&p.state)),
        bundled_cli: daemon_proc::bundled_cli(state.exe.as_deref())
            .map(|p| proto::text::escape_path(&p)),
    }
}

#[derive(Serialize)]
struct DaemonState {
    running: bool,
    message: Option<String>,
    notice: Option<String>,
}

fn daemon_state_of(state: &AppState) -> DaemonState {
    let ping = state.client.ping();
    DaemonState {
        running: ping.is_ok(),
        message: ping.err().map(message),
        notice: state.notice.lock().unwrap_or_else(|e| e.into_inner()).clone(),
    }
}

#[tauri::command]
async fn daemon_state(state: State<'_, Arc<AppState>>) -> Result<DaemonState, String> {
    let state = Arc::clone(&state);
    blocking(move || Ok(daemon_state_of(&state))).await
}

/// Starts the daemon when nothing answers. With `restart`, stops the one that
/// is answering and starts it again, as the menu bar icon's item does.
#[tauri::command]
async fn start_daemon(
    app: AppHandle,
    restart: Option<bool>,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let state = Arc::clone(&state);
    let result = blocking(move || state.start_daemon(restart.unwrap_or(false))).await;
    refresh_tray(&app);
    result
}

#[tauri::command]
async fn list_projects(state: State<'_, Arc<AppState>>) -> Result<Vec<ProjectRow>, String> {
    let state = Arc::clone(&state);
    blocking(move || {
        let paths = state.paths()?;
        let running = state.client.ping().is_ok();
        Ok(ops::list_projects(&state.client, paths, running))
    })
    .await
}

/// Points the daemon at a folder. The daemon records any project it is asked
/// about, which is how it joins the list.
#[tauri::command]
async fn add_project(path: String, state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let state = Arc::clone(&state);
    blocking(move || {
        let path = path.trim().to_owned();
        if !std::path::Path::new(&path).is_absolute() {
            return Err("Give the project folder's full path, starting with /.".to_owned());
        }
        match ops::status(&state.client, &path) {
            Ok(_) => Ok("Added.".to_owned()),
            // The daemon records a project before it asks the engine about it,
            // so an engine failure still leaves it in the list.
            Err(error) => {
                let recorded = std::fs::canonicalize(&path).ok().is_some_and(|canonical| {
                    let canonical = canonical.to_string_lossy();
                    state.paths().is_ok_and(|paths| {
                        hoststate::known_projects(paths)
                            .iter()
                            .any(|(_, record)| record.path == canonical)
                    })
                });
                if recorded {
                    Ok(format!(
                        "Added, but the daemon could not read its state: {}",
                        error.message()
                    ))
                } else {
                    Err(error.message())
                }
            }
        }
    })
    .await
}

#[tauri::command]
async fn import_remote(
    url: String,
    git_ref: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<ops::ImportView, String> {
    let state = Arc::clone(&state);
    blocking(move || {
        let url = url.trim().to_owned();
        if url.is_empty() {
            return Err("Give an SSH remote, such as git@github.com:org/repo.git.".to_owned());
        }
        // An empty field means the remote's default branch.
        let git_ref = git_ref.map(|r| r.trim().to_owned()).filter(|r| !r.is_empty());
        ops::import_remote(&state.client, &url, git_ref, ops::agent_socket()).map_err(message)
    })
    .await
}

/// One Git operation on a project imported from a remote: `status`, `pull`,
/// `commit` (with a message), `push`, or `run` with any Git command's
/// arguments. It runs inside the sandbox. Fetch, pull and push use the
/// person's SSH key on this machine, through the daemon's relay, which checks
/// every push.
#[tauri::command]
async fn git(
    project: String,
    op: String,
    commit_message: Option<String>,
    args: Option<Vec<String>>,
    state: State<'_, Arc<AppState>>,
) -> Result<ops::GitView, String> {
    let state = Arc::clone(&state);
    blocking(move || {
        let op = match op.as_str() {
            "status" => proto::GitOp::Status,
            "pull" => proto::GitOp::Pull,
            "push" => proto::GitOp::Push,
            "commit" => proto::GitOp::Commit {
                message: commit_message.unwrap_or_default(),
                paths: Vec::new(),
            },
            "run" => proto::GitOp::Run { args: args.unwrap_or_default() },
            _ => return Err("That is not a Git operation this window offers.".to_owned()),
        };
        ops::git(&state.client, &project, op, ops::host_identity(), ops::agent_socket()).map_err(message)
    })
    .await
}

#[derive(Serialize)]
struct StatusReply {
    #[serde(flatten)]
    view: StatusView,
    log_services: Vec<String>,
}

#[tauri::command]
async fn status(project: String, state: State<'_, Arc<AppState>>) -> Result<StatusReply, String> {
    let state = Arc::clone(&state);
    blocking(move || {
        let report = ops::status(&state.client, &project).map_err(message)?;
        Ok(StatusReply {
            view: ops::status_view(&report),
            log_services: ops::log_services(&report),
        })
    })
    .await
}

#[tauri::command]
async fn plan(project: String, state: State<'_, Arc<AppState>>) -> Result<PlanView, String> {
    let state = Arc::clone(&state);
    blocking(move || {
        let report = ops::plan(&state.client, &project).map_err(message)?;
        if report.blockers.is_empty() {
            state.plans.remember(&project, &report);
        }
        Ok(ops::plan_view(&report))
    })
    .await
}

#[derive(Clone, Serialize)]
struct ProgressEvent {
    step: String,
    detail: Option<String>,
}

#[tauri::command]
async fn up(
    app: AppHandle,
    project: String,
    digest: String,
    ports: Vec<u16>,
    on_progress: Channel<ProgressEvent>,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let state = Arc::clone(&state);
    let result = blocking(move || {
        state.plans.take_matching(&project, &digest, &ports)?;
        ops::up(&state.client, &project, &digest, ports, &mut |step, detail| {
            let _ = on_progress.send(ProgressEvent {
                step: proto::text::escape(step),
                detail: detail.map(proto::text::escape),
            });
        })
        .map_err(message)
    })
    .await;
    refresh_tray(&app);
    result
}

#[tauri::command]
async fn down(app: AppHandle, project: String, state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let state = Arc::clone(&state);
    let result = blocking(move || ops::down(&state.client, &project, false).map_err(message)).await;
    refresh_tray(&app);
    result
}

#[tauri::command]
async fn destroy(app: AppHandle, project: String, state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let state = Arc::clone(&state);
    let result = blocking(move || ops::down(&state.client, &project, true).map_err(message)).await;
    refresh_tray(&app);
    result
}

#[tauri::command]
async fn approve_host(
    app: AppHandle,
    project: String,
    host: String,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let state = Arc::clone(&state);
    let result = blocking(move || ops::approve_host(&state.client, &project, &host).map_err(message)).await;
    refresh_tray(&app);
    result
}

#[tauri::command]
async fn remove_project(app: AppHandle, project: String, state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let state = Arc::clone(&state);
    let result = blocking(move || ops::remove(&state.client, &project).map_err(message)).await;
    refresh_tray(&app);
    result
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LogEvent {
    Text { text: String },
    End { note: Option<String> },
}

/// Starts reading logs and returns an id that `logs_stop` takes. The text
/// arrives on the channel, already filtered.
#[tauri::command]
fn logs(
    project: String,
    service: Option<String>,
    follow: bool,
    on_event: Channel<LogEvent>,
    state: State<'_, Arc<AppState>>,
) -> Result<u64, String> {
    if let Some(name) = &service {
        proto::check_service_name(name).map_err(str::to_owned)?;
    }
    let id = state.next_log.fetch_add(1, Ordering::Relaxed);
    let stopper = Stopper::default();
    state
        .logs
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, stopper.clone());
    let state = Arc::clone(&state);
    std::thread::spawn(move || {
        let result = ops::logs(&state.client, &project, service, follow, &stopper, &mut |text| {
            on_event.send(LogEvent::Text { text: text.to_owned() }).is_ok()
        });
        let _ = on_event.send(LogEvent::End {
            note: ops::logs_end(&result),
        });
        state.logs.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    });
    Ok(id)
}

#[tauri::command]
fn logs_stop(id: u64, state: State<'_, Arc<AppState>>) {
    if let Some(stopper) = state.logs.lock().unwrap_or_else(|e| e.into_inner()).remove(&id) {
        stopper.stop();
    }
}

#[tauri::command]
async fn blocked(project: String, state: State<'_, Arc<AppState>>) -> Result<BlockedView, String> {
    let state = Arc::clone(&state);
    blocking(move || {
        ops::blocked(&state.client, &project)
            .map(|r| ops::blocked_view(&r))
            .map_err(message)
    })
    .await
}

/// Compares the project's running sandbox with the snapshot it was given.
/// Nothing is copied out: the daemon reviews a bundle and sends the findings.
#[tauri::command]
async fn diff(project: String, state: State<'_, Arc<AppState>>) -> Result<DiffView, String> {
    let state = Arc::clone(&state);
    blocking(move || {
        ops::diff(&state.client, &project)
            .map(|r| ops::diff_view(&r))
            .map_err(message)
    })
    .await
}

/// What the menu bar icon shows.
#[derive(Clone, PartialEq, Eq)]
struct TraySnapshot {
    running: bool,
    projects: Vec<(String, &'static str)>,
    more: usize,
}

fn tray_snapshot(state: &AppState) -> TraySnapshot {
    let running = state.client.ping().is_ok();
    let rows = match state.paths() {
        Ok(paths) => ops::list_projects(&state.client, paths, running),
        Err(_) => Vec::new(),
    };
    TraySnapshot {
        running,
        more: rows.len().saturating_sub(TRAY_PROJECTS),
        projects: rows
            .into_iter()
            .take(TRAY_PROJECTS)
            .map(|r| (r.name, r.label))
            .collect(),
    }
}

fn tray_menu(app: &AppHandle, snap: &TraySnapshot) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;
    let daemon_line = if snap.running {
        format!("{}: running", brand::DAEMON_BIN)
    } else {
        format!("{}: not running", brand::DAEMON_BIN)
    };
    menu.append(&MenuItem::with_id(app, "daemon-line", daemon_line, false, None::<&str>)?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    if snap.projects.is_empty() {
        menu.append(&MenuItem::with_id(app, "no-projects", "No projects yet", false, None::<&str>)?)?;
    }
    for (index, (name, label)) in snap.projects.iter().enumerate() {
        // Menu text is not markup, but keep it short and plain anyway.
        let short: String = name.chars().take(40).collect();
        menu.append(&MenuItem::with_id(
            app,
            format!("project-{index}"),
            format!("{short}  \u{2014}  {label}"),
            false,
            None::<&str>,
        )?)?;
    }
    if snap.more > 0 {
        menu.append(&MenuItem::with_id(app, "more", format!("and {} more", snap.more), false, None::<&str>)?)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "open", "Open window", true, None::<&str>)?)?;
    let (id, label) = if snap.running {
        ("restart-daemon", "Restart daemon")
    } else {
        ("start-daemon", "Start daemon")
    };
    menu.append(&MenuItem::with_id(app, id, label, true, None::<&str>)?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Quit (the daemon keeps running)", true, None::<&str>)?)?;
    Ok(menu)
}

fn apply_tray(app: &AppHandle, snap: &TraySnapshot) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    if let Ok(menu) = tray_menu(app, snap) {
        let _ = tray.set_menu(Some(menu));
    }
    let icon = if snap.running {
        tauri::include_image!("icons/tray-on.png")
    } else {
        tauri::include_image!("icons/tray-off.png")
    };
    let _ = tray.set_icon(Some(icon));
    let tip = if snap.running {
        format!("{}: daemon running", brand::PRODUCT_NAME)
    } else {
        format!("{}: daemon not running", brand::PRODUCT_NAME)
    };
    let _ = tray.set_tooltip(Some(tip));
}

/// Reads the state again and redraws the menu, off the main thread.
fn refresh_tray(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<Arc<AppState>>();
        let snap = tray_snapshot(&state);
        apply_tray(&app, &snap);
    });
}

fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(WINDOW) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn on_menu(app: &AppHandle, id: &str) {
    match id {
        "open" => show_window(app),
        "start-daemon" | "restart-daemon" => {
            let restart = id == "restart-daemon";
            let app = app.clone();
            std::thread::spawn(move || {
                let state = app.state::<Arc<AppState>>();
                let result = state.start_daemon(restart);
                state.set_notice(match result {
                    Ok(text) => text,
                    Err(text) => text,
                });
                let snap = tray_snapshot(&state);
                apply_tray(&app, &snap);
            });
        }
        "quit" => app.exit(0),
        _ => {}
    }
}

pub fn run() {
    let app = tauri::Builder::default()
        .setup(|app| {
            let log_dir = app
                .path()
                .app_log_dir()
                .unwrap_or_else(|_| std::env::temp_dir());
            let log = log_dir.join(format!("{}.log", brand::DAEMON_BIN));
            let state = Arc::new(AppState::new(log));
            app.manage(Arc::clone(&state));

            let handle = app.handle().clone();
            let initial = TraySnapshot {
                running: false,
                projects: Vec::new(),
                more: 0,
            };
            TrayIconBuilder::with_id(TRAY_ID)
                .icon(tauri::include_image!("icons/tray-off.png"))
                .tooltip(brand::PRODUCT_NAME)
                .menu(&tray_menu(&handle, &initial)?)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
                .build(app)?;

            // The daemon is the part that must always be there. If nothing
            // answers, start it, and say so in the window.
            std::thread::spawn(move || {
                let state = handle.state::<Arc<AppState>>();
                if state.client.ping().is_err() {
                    let text = match state.start_daemon(false) {
                        Ok(text) => text,
                        Err(reason) => format!("No daemon is running, and it could not be started: {reason}"),
                    };
                    state.set_notice(text);
                }
                let mut last: Option<TraySnapshot> = None;
                loop {
                    let snap = tray_snapshot(&state);
                    if last.as_ref() != Some(&snap) {
                        apply_tray(&handle, &snap);
                        last = Some(snap);
                    }
                    std::thread::sleep(TRAY_REFRESH);
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps the app in the menu bar.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            daemon_state,
            start_daemon,
            list_projects,
            add_project,
            status,
            plan,
            up,
            down,
            destroy,
            remove_project,
            approve_host,
            logs,
            logs_stop,
            blocked,
            import_remote,
            git,
            diff,
        ])
        .build(tauri::generate_context!())
        .expect("the app could not start");

    app.run(|_app, event| {
        // Without a window open the app still runs; only Quit ends it.
        if let RunEvent::ExitRequested { api, code: None, .. } = event {
            api.prevent_exit();
        }
    });
}
