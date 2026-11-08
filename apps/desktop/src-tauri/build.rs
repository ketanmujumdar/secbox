// Declares the window's commands to Tauri. With a manifest, a command is
// refused unless a capability file grants it, so this list and the one in
// `capabilities/main.json` are the whole surface the web view can reach.

const COMMANDS: &[&str] = &[
    "app_info",
    "daemon_state",
    "start_daemon",
    "list_projects",
    "add_project",
    "status",
    "plan",
    "up",
    "down",
    "destroy",
    "remove_project",
    "approve_host",
    "connect_agent",
    "open_in_vscode",
    "logs",
    "logs_stop",
    "blocked",
    "import_remote",
    "git",
    "diff",
    "secrets",
    "set_secret",
    "secret_files",
    "read_secret_file",
    "use_secret_file",
    "problems",
    "resource_choices",
    "set_resources",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("the Tauri build step failed");
}
