// No console window on Windows release builds. Nothing here targets Windows
// yet, but this is the usual Tauri entry point.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    desktop::run();
}
