# secbox desktop app

**Early build. Don't rely on it.** It is one more client of the daemon, and the daemon itself has not been shown to contain anything yet. See the status section in the [root README](../../README.md).

The app and the terminal are bundled together. Anyone who wants a terminal uses `secbox` (or `secbox` with no arguments for the full-screen interface). The app adds two things:

- **A menu bar icon** (tray icon on Linux), which is the part that is always there. It shows whether `secboxd` is answering and lists known projects with their state, with *Open window*, *Start daemon* or *Restart daemon*, and *Quit*. Closing the window keeps the app in the menu bar. Quitting the app leaves the daemon running.
- **A window**, described below.

The app has no powers of its own.

## The window

A bar along the top carries the red *Early build · not protecting anything yet* tag. It never goes away.

**With no projects**, the whole window asks how to bring one in: *Clone from Git* (recommended) or *Add a folder*. The daemon's state is under the two choices.

Bringing a project in takes three pages:

1. **Where is it.** For Git, an SSH URL and an optional branch, tag or commit. It is fetched on this computer with your SSH key, and your SSH agent may ask you to approve. For a folder, its full path. Only committed files go in, and the folder isn't changed. There is no file picker: the app has no file system plugin, so the path is typed.
2. **Review.** The daemon's plan, grouped as: sites it can reach while building and while running; ports it would open on 127.0.0.1, each with a checkbox; secrets it gets (names and services, then where they could be sent); and how much memory, CPU and disk it can use. Findings and files that look like secrets follow in an amber box. Blockers, when there are any, replace *Start* with a red box saying why. The plan's digest is at the bottom, and *Show the full plan* opens the services, the commit, any Compose file written for a Dockerfile and the whole digest. *Cancel* has the focus.
3. **Starting.** A checklist of the steps the daemon reports while it starts the sandbox (checking the plan, the VM, the snapshot, Docker, secrets, the build), with the time each took, then the ports opened. Steps the daemon skips are dropped, not ticked. The page can be left; the project shows *starting* on the left until `up` returns.

**With projects**, a rail on the left has *+ New project* (the same two choices), the project list with each one's state, and at the bottom the daemon's state with *Start it* or *Restart*, and *About*. The open project has a header with its name, state, path (or, for one cloned from Git, the copy the daemon keeps), *Start* or *Stop*, and a ••• menu with *Restart with a new plan*, *Destroy sandbox* and *Remove from list*. Its tabs:

- **Overview**: services and their state, limits (what it may use, not what it uses), ports on 127.0.0.1, the blocked-hostname count with the newest three, the last comparison if one was made in this window, and where its secrets could go.
- **Logs**: one chip per service, follow or read once. Control codes are removed and assigned secret values arrive as their names.
- **Network**: blocked hostnames, newest first, with the notes on what this list does not cover. The tab shows how many hostnames were blocked.
- **Changes**: compares the running sandbox with its snapshot when you press *Compare*, and lists flags first, then every changed file, as `diff` does. Nothing is copied out. For a project cloned from Git, *Commit all changes*, *Status*, *Pull* and *Push branch* sit beside it. The tab shows how many files changed once compared.
- **Secrets**: where this project's secrets could go, and the terminal commands to manage them. Values are never shown.
- **Settings**: the project's path and where it came from, its limits, and *Destroy* and *Remove*.

What the mockups showed but the daemon does not report is left out: live CPU and memory use, how long it has been running, the remote URL and branch of a cloned project, an AI agent's last call, choosing files for a commit, moving secrets from a `.env` into the keychain, and making a folder read-only. *Open shell* is the terminal's `secbox shell`; the window has no terminal. Every button becomes one request on the daemon's owner-only Unix socket, the same request the CLI sends, and the daemon applies the same rules.

## How it is put together

```
apps/desktop/
  ui/                    static HTML, CSS and JS, no build step
  ui-preview/            a stand-in for the app, to look at ui/ in a browser; never bundled
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
- **Strict CSP**: `default-src 'self'`, no inline script or style, no remote anything. `freezePrototype` is on. The page has no `style` attributes and uses the system's fonts.
- **Plain text only.** Everything from the daemon or a sandbox goes into the page with `textContent`. Nothing uses `innerHTML`. The Rust side also escapes every string with `proto::text::escape`, and filters logs with `TerminalFilter`, as the CLI does.
- **Narrow commands.** `build.rs` declares the app's commands, so Tauri refuses any command a capability file does not grant. `capabilities/main.json` grants exactly these to the main window, and no core or plugin permission: `app_info`, `daemon_state`, `start_daemon` (which can also restart it, as the menu bar item does), `list_projects`, `add_project`, `remove_project`, `status`, `plan`, `up`, `down`, `destroy`, `logs`, `logs_stop`, `blocked`, `import_remote` (an SSH URL and an optional ref, which the daemon checks before fetching), `git`, `diff`. No file system, shell, HTTP or opener plugin is installed.
- **Start needs the plan that was shown.** *Start* asks the daemon for a plan and shows it on the Review page: allowed hosts, ports to forward, secret names and where they could go, limits, findings, services, commit. The terminal's start-box lines come first, except the ports line, which the Ports card shows with a checkbox per port. `up` is sent only after *Start with this plan* is clicked. It carries that plan's digest and the ports still ticked, which can only be ports from that plan. The Rust side also keeps the digest it handed out and refuses an `up` that names any other digest, or a port outside that plan, before the daemon makes its own check. Cancel is focused, so Enter never agrees by accident.
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

## Looking at the window in a browser

`ui-preview/` is for working on the page without building the app. `stub.js` stands in for `window.__TAURI__` and answers every command with made-up data in the shapes `src-tauri/src/ops.rs` and `lib.rs` send. `server.js` serves `ui/` on 127.0.0.1 with that stub in front of `app.js` and the app's CSP as a header. `shoot.js` drives it in Chromium with Playwright, through first run, cloning, the review, starting, and each tab, in light and dark, and fails on a console error, a page error, a CSP violation, a `style` attribute, an `up` that does not carry the shown plan's digest and ticked ports, or a confirmation without *Cancel* focused:

```sh
node apps/desktop/ui-preview/server.js            # then open http://127.0.0.1:8765/?scene=projects
node apps/desktop/ui-preview/shoot.js /tmp/shots  # needs require('playwright') and its Chromium
```

Scenes are `empty`, `projects`, `stopped`, `starting` (a start that never finishes) and `nodaemon`. None of this is bundled: `frontendDist` is `../ui`, and the server only ever runs by hand.

## Getting the CLI onto your PATH

The `.app` contains the CLI at `secbox.app/Contents/MacOS/secbox`. To use it from a terminal, make a link once:

```sh
sudo ln -sf /Applications/secbox.app/Contents/MacOS/secbox /usr/local/bin/secbox
# or, without admin rights, if ~/.local/bin is on your PATH:
mkdir -p ~/.local/bin && ln -sf /Applications/secbox.app/Contents/MacOS/secbox ~/.local/bin/secbox
```

The window's *About* page (bottom of the left rail) shows these commands with this copy's real path.

This is a documented step rather than a menu item. A menu item would need the app to write into a system folder with admin rights, through a privileged helper or a password prompt it draws itself. That is more machinery and more trust than one `ln` you can read before you run it. The link points into the bundle, so it keeps working when the app updates in place. The Homebrew formula in `packaging/homebrew` is unchanged and remains the other way to get the CLI. On Linux the `.deb` installs `secbox`, `secboxd` and `secbox-desktop` into `/usr/bin`, so nothing extra is needed there.

The app's own executable is named `secbox-desktop` (`mainBinaryName`), so it cannot collide with the `secbox` CLI next to it.

## What works and what has been seen

Run in a Linux container (x86_64, WebKitGTK 2.52) under Xvfb, with a real `secboxd`, a temporary `SECBOX_HOME`, and no engine (`SECBOX_ENGINE_BIN=/bin/false`):

- At launch with no daemon, the app found `secboxd` beside its executable, started it, and showed the notice. The daemon was in its own process group and kept running when the app was killed. On the next launch the app found it answering and did not start a second one.
- Adding a project folder by path put it in the list. The daemon's status error ("msb status exited with 1", because there was no engine) was shown as plain text.
- *Start* showed the plan's blockers (CPU, then disk) when the plan could not start, and the full plan once the project's limits fitted. Confirming sent `up` with the digest. The daemon's refusal was shown.
- The *Destroy* confirmation, the Logs and Blocked traffic tabs, and *Terminal & about* rendered as expected.
- Stopping the daemon by hand turned the indicator red and disabled the buttons. *Start it* in the window brought the daemon back.
- The window as it is now (rail, import pages, project tabs) was clicked through in Chromium against `ui-preview`, at 1280x800 in light and dark, with no console errors or CSP violations. In the real app under Xvfb (WebKitGTK, real `secboxd`, no engine), the first-run page, *Add a folder*, the Review page with the daemon's real plan and its disk blocker, Enter on the focused *Close* (nothing started), the project header, the Network tab and the ••• menu rendered as expected. Two requests at once for a newly added project can make the daemon answer "cannot create project.json: entity already exists"; the menu bar icon and the window both ask, so the window can show it. The CLI does the same when run twice at once. The rest of the list above was seen with the earlier window, whose sections were tabs above the project list.
- `cargo test` in `src-tauri` passes, except `a_review_reaches_the_daemon_and_its_refusals_read_plainly`, which expects the daemon to say "no bundle" and now gets its newer refusal about a missing record of what the sandbox started from. That test failed before the window was redesigned too. It drives `plan`, `up` with the digest, `status`, `down`, `destroy`, logs (with control codes removed, and a followed log stopped from another thread) and blocked hostnames against the real daemon server on a socket, with the fake engine. It also checks the product name rule for Rust code here, which the root workspace's guard does not cover.

## Not verified

- **The current window on a Mac.** It has been seen in WebKitGTK and Chromium, not in WKWebView. Cloning, the Starting checklist with real progress, Logs, Changes and Git have been driven only against the stand-in.

- **macOS.** Nothing has been built or run on a Mac: not the `.app`, the `.dmg`, the menu bar icon, code signing or notarization (neither is set up), or the `LocalPeerPid` path that *Restart daemon* uses there.
- **The menu bar icon and its menu.** Xvfb has no tray host, so the icon and menu were never seen. *Restart daemon*, the project list in the menu and *Quit* have not been clicked. The window's own *Start it* uses the same start code as the menu.
- **Closing the window to the tray.** Xvfb has no window manager to send a close request, so this was not exercised.
- **A running sandbox.** No engine was available, so no project was started. Services, ports, resources, real log output and real blocked events have been rendered only from the fake engine in tests, not in the window.
- **AppImage** was not built. A debug `.deb` was built with `cargo tauri build --debug --bundles deb`, and its contents list `usr/bin/secbox`, `usr/bin/secboxd` and `usr/bin/secbox-desktop`, but it was not installed. No release build was made.
- **The `.app` launched from Finder.** It gets a minimal `PATH`, so it relies on the bundled `secboxd`, and the daemon then reads `HOME` from launchd. That should be right, but it has not been checked.
