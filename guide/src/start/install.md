# Install

secbox is built from source for now. This page installs the command line tool, the daemon and the VM engine, then checks the result.

> There is no release, Homebrew tap or package yet. Every step here builds from the repository.

## Requirements

| What | Why |
| --- | --- |
| macOS on Apple Silicon, or Linux with `/dev/kvm` | The VM uses the hypervisor built into the OS |
| A Rust toolchain with Cargo | To build from source. Install it from [rustup.rs](https://rustup.rs) |
| On Linux: `pkg-config` and the D-Bus headers | The keychain library talks to Secret Service over D-Bus |
| About 8 GB of memory, 16 GB recommended | An estimate, not a measurement. A typical Compose project needed a 3 GB VM to build |
| Git 2.50 or newer | `up` copies a project into the VM from a Git commit, and needs Git 2.50 to do it safely |

Docker is **not** needed on your machine. Each VM has its own.

On Debian or Ubuntu:

```sh
sudo apt install build-essential pkg-config libdbus-1-dev
```

On Fedora:

```sh
sudo dnf install gcc pkgconf-pkg-config dbus-devel
```

## 1. Get the code

```sh
git clone https://github.com/ketanmujumdar/secbox.git
cd secbox
```

## 2. Install the command line tool

```sh
cargo install --path crates/cli --locked
```

This puts `secbox` in `~/.cargo/bin`. If you use asdf, run `asdf reshim rust` afterwards.

## 3. Install the daemon

`up`, `down`, `status`, `logs`, `shell`, `exec`, `net blocked` and the MCP server all talk to a daemon, `secboxd`. It holds the secrets, the policy and the approvals.

```sh
cargo install --path crates/daemon --locked
```

## 4. Install the VM engine

secbox drives [microsandbox](https://github.com/superradcompany/microsandbox) (`msb`) to create the VMs. It has been tested with **v0.7.13** only. Download that release from the microsandbox GitHub releases page, verify its checksums, and then either:

- put it at `~/.secbox/msb/bin/msb`, which is where secbox looks by default, or
- set `SECBOX_ENGINE_BIN` to the absolute path of `msb` when you start the daemon.

secbox never looks for `msb` on your `PATH` or in the current folder, so a binary planted in a project cannot be picked up by accident.

## 5. Check the install

```sh
secbox --version
secbox doctor
```

`doctor` checks the platform, the hypervisor, CPU, memory, disk, the state folder, the socket path length and the engine. Each line says `ok`, `warn` or `fail`, and a `next` line under a problem says what to do. Sample output on a Mac:

```text
Platform      ok    macOS 26.5.1 on aarch64
Hypervisor    ok    Hypervisor.framework is supported
CPU           ok    12 logical CPUs
Memory        ok    48.0 GB total, about 9.0 GB free
Disk          ok    89.3 GB free
State folder  ok    /Users/you/.secbox, not created yet
Socket path   ok    daemon 32 bytes, engine 73 bytes, must be under 104
Engine        ok    msb 0.7.13
Suggested           4G memory, 2 CPUs, 20G disk

Failed: 0. Warnings: 0.
```

`doctor` exits with code 1 if any line failed.

## 6. Start the daemon

```sh
secboxd
```

Leave it running in its own terminal. With a custom engine path:

```sh
SECBOX_ENGINE_BIN=/absolute/path/to/msb secboxd
```

The [desktop app](../guides/desktop-app.md) can start and keep the daemon running for you instead.

## Trying it without touching your real state

Set `SECBOX_HOME` to an empty folder with a short path. secbox then keeps everything in `$SECBOX_HOME/.secbox` instead of `~/.secbox`:

```sh
mkdir -p /tmp/sbtest
export SECBOX_HOME=/tmp/sbtest
```

Keep the path short. Unix socket paths have a length limit of about 104 bytes on macOS and 108 on Linux, and `doctor` fails the `Socket path` line if the path is too long. The default macOS temporary folder is too long.

To install the binaries somewhere other than `~/.cargo/bin`:

```sh
cargo install --path crates/cli --locked --root /tmp/secbox-install
export PATH="/tmp/secbox-install/bin:$PATH"
```

## Next

[Quick start](quickstart.md).
