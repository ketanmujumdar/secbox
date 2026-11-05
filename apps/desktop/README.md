# secbox desktop app

**Early build. Don't rely on it.** It is one more client of the daemon, and the daemon itself has not been shown to contain anything yet. See the status section in the [root README](../../README.md).

The app and the terminal are bundled together. Anyone who wants a terminal uses `secbox` (or `secbox` with no arguments for the full-screen interface). The app adds two things:

- **A menu bar icon** (tray icon on Linux), which is the part that is always there. It shows whether `secboxd` is answering and lists known projects with their state, with *Open window*, *Start daemon* or *Restart daemon*, and *Quit*. Closing the window keeps the app in the menu bar. Quitting the app leaves the daemon running.
- **A window**: an overview of each project (state, services, ports on 127.0.0.1, resources, blocked-traffic count), *Start*, *Stop* and *Destroy*, a Logs tab and a Blocked traffic tab. Secrets and Git review show the same "not built yet" text as the terminal interface.

The app has no powers of its own. Every button becomes one request on the daemon's owner-only Unix socket, the same request the CLI sends, and the daemon applies the same rules.

## How it is put together

```
apps/desktop/
  ui/                    static HTML, CSS and JS, no build step
  src-tauri/             the Rust side, its own Cargo workspace
    src/client.rs        a small client on proto::wire, with a way to stop a followed log
    src/ops.rs           requests and the views made from the answers, no Tauri in it
    src/daemon_proc.rs   finding, starting and restarting secboxd
    src/lib.rs           Tauri commands, the menu bar icon and the window
    capabilities/        the only commands the window may call
    tauri.conf.json      window, CSP and bundle settings
    tests/               tests against the real daemon server, with the fake engine
  scripts/prepare-sidecars.sh
```

It is a separate Cargo workspace (the root `Cargo.toml` excludes `apps`) because Tauri needs WebKit to build, and `cargo test --workspace` at the root must keep working without it. It depends on the `brand`, `proto` and `hoststate` crates by path.

It uses its own small client rather than the CLI's `SocketClient`. That one lives in the `cli` crate, which would bring the engine, the policy engine, clap and the password prompt with it, and it has no way to stop a call from another thread, which a followed log in a window needs.

### Security choices

- **No local web port.** The page is served by Tauri's built-in asset protocol from `ui/`. There is no dev server and no `devUrl`, so nothing listens on localhost.
- **No front-end toolchain.** Plain HTML, CSS and JS. No npm, Vite or bundler, so no package tree to trust.
- **Strict CSP**: `default-src 'self'`, no inline script or style, no remote anything. `freezePrototype` is on.
- **Plain text only.** Everything from the daemon or a sandbox goes into the page with `textContent`. Nothing uses `innerHTML`. The Rust side also escapes every string with `proto::text::escape`, and filters logs with `TerminalFilter`, as the CLI does.
- **Narrow commands.** `build.rs` declares the app's commands, so Tauri refuses any command a capability file does not grant. `capabilities/main.json` grants exactly these to the main window, and no core or plugin permission: `app_info`, `daemon_state`, `start_daemon`, `list_projects`, `add_project`, `status`, `plan`, `up`, `down`, `destroy`, `logs`, `logs_stop`, `blocked`. No file system, shell, HTTP or opener plugin is installed.
- **Start needs the plan that was shown.** *Start* asks the daemon for a plan and shows it: services, commit, ports to forward, allowed hosts, secret names, limits, findings. The same summary lines as the terminal's start box come first. `up` is sent only after *Start with this plan* is clicked. It carries that plan's digest and its ports. The Rust side also keeps the digest it handed out and refuses an `up` that names any other digest, or a port outside that plan, before the daemon makes its own check. Cancel is focused, so Enter never agrees by accident.
- **Destroy** needs its own confirmation, and afterwards says to rotate every secret the sandbox was given.
- **Starting the daemon.** If nothing answers on the socket at launch, the app starts `secboxd` and says so in the window. It looks beside its own executable first, where the bundled copy is, then on `PATH`, skipping relative entries. The daemon runs in its own process group with no arguments, so no secret is ever on a command line, and it outlives the app. Its output goes to the app's log folder (`secboxd.log`, owner-only). *Restart daemon* sends SIGTERM to the process listening on the socket, found from the socket's peer credentials, waits for it to stop, then starts it again. Sandboxes are separate processes and keep running.
- **Approval prompts** (Touch ID and the like) are not part of this app. They belong to the daemon and the OS, so a page cannot click them.

## Build and run

You need Rust, and on Linux WebKitGTK:

```sh
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev build-essential pkg-config libdbus-1-dev
cargo install tauri-cli --version '^2' --locked
```

On macOS, Xcode's command line tools are enough.

The bundle carries `secbox` and `secboxd` as Tauri sidecars, and Tauri refuses to build until they exist. Build them first:

```sh
apps/desktop/scripts/prepare-sidecars.sh            # release builds
apps/desktop/scripts/prepare-sidecars.sh --debug    # faster, for trying it out
```

This builds both binaries from the root workspace and copies them to `src-tauri/binaries/<name>-<target triple>`. That folder is git-ignored.

Then:

```sh
cd apps/desktop/src-tauri
cargo tauri dev                 # or: cargo build && target/debug/desktop
cargo test                      # unit tests, and tests against the daemon with the fake engine
cargo tauri build               # macOS: .app and .dmg; Linux: .deb and .AppImage
cargo tauri build --bundles app # just the .app
```

`cargo tauri dev` serves `ui/` from disk through the asset protocol. It starts no web server. A plain `cargo build` also works, and copies the sidecars beside `target/<profile>/desktop`.

To try it without touching your real state, set `SECBOX_HOME` to an empty folder before launching. The app and the daemon it starts use `$SECBOX_HOME/.secbox`.

## Getting the CLI onto your PATH

The `.app` contains the CLI at `secbox.app/Contents/MacOS/secbox`. To use it from a terminal, make a link once:

```sh
sudo ln -sf /Applications/secbox.app/Contents/MacOS/secbox /usr/local/bin/secbox
# or, without admin rights, if ~/.local/bin is on your PATH:
mkdir -p ~/.local/bin && ln -sf /Applications/secbox.app/Contents/MacOS/secbox ~/.local/bin/secbox
```

The window's *Terminal & about* tab shows these commands with this copy's real path.

This is a documented step rather than a menu item. A menu item would need the app to write into a system folder with admin rights, through a privileged helper or a password prompt it draws itself. That is more machinery and more trust than one `ln` you can read before you run it. The link points into the bundle, so it keeps working when the app updates in place. The Homebrew formula in `packaging/homebrew` is unchanged and remains the other way to get the CLI. On Linux the `.deb` installs `secbox`, `secboxd` and `secbox-desktop` into `/usr/bin`, so nothing extra is needed there.

The app's own executable is named `secbox-desktop` (`mainBinaryName`), so it cannot collide with the `secbox` CLI next to it.

## What works and what has been seen

Run in a Linux container (x86_64, WebKitGTK 2.52) under Xvfb, with a real `secboxd`, a temporary `SECBOX_HOME`, and no engine (`SECBOX_ENGINE_BIN=/bin/false`):

- At launch with no daemon, the app found `secboxd` beside its executable, started it, and showed the notice. The daemon was in its own process group and kept running when the app was killed. On the next launch the app found it answering and did not start a second one.
- Adding a project folder by path put it in the list. The daemon's status error ("msb status exited with 1", because there was no engine) was shown as plain text.
- *Start* showed the plan's blockers (CPU, then disk) when the plan could not start, and the full plan once the project's limits fitted. Confirming sent `up` with the digest. The daemon's refusal was shown.
- The *Destroy* confirmation, the Logs and Blocked traffic tabs, and *Terminal & about* rendered as expected.
- Stopping the daemon by hand turned the indicator red and disabled the buttons. *Start it* in the window brought the daemon back.
- `cargo test` in `src-tauri` passes. It drives `plan`, `up` with the digest, `status`, `down`, `destroy`, logs (with control codes removed, and a followed log stopped from another thread) and blocked hostnames against the real daemon server on a socket, with the fake engine. It also checks the product name rule for Rust code here, which the root workspace's guard does not cover.

## Not verified

- **macOS.** Nothing has been built or run on a Mac: not the `.app`, the `.dmg`, the menu bar icon, code signing or notarization (neither is set up), or the `LocalPeerPid` path that *Restart daemon* uses there.
- **The menu bar icon and its menu.** Xvfb has no tray host, so the icon and menu were never seen. *Restart daemon*, the project list in the menu and *Quit* have not been clicked. The window's own *Start it* uses the same start code as the menu.
- **Closing the window to the tray.** Xvfb has no window manager to send a close request, so this was not exercised.
- **A running sandbox.** No engine was available, so no project was started. Services, ports, resources, real log output and real blocked events have been rendered only from the fake engine in tests, not in the window.
- **AppImage** was not built. A debug `.deb` was built with `cargo tauri build --debug --bundles deb`, and its contents list `usr/bin/secbox`, `usr/bin/secboxd` and `usr/bin/secbox-desktop`, but it was not installed. No release build was made.
- **The `.app` launched from Finder.** It gets a minimal `PATH`, so it relies on the bundled `secboxd`, and the daemon then reads `HOME` from launchd. That should be right, but it has not been checked.
