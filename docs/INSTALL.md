# Installing secbox

secbox is an early build. Do not rely on it for untrusted code. See the status in the README.

## Requirements

- macOS on Apple Silicon, or Linux with KVM (`/dev/kvm`). `secbox doctor` checks this.
- A Rust toolchain with Cargo, to build from source.
- Docker is not needed.

The checks in `doctor` follow the design's estimates of 8 GB of memory as a minimum and 16 GB as recommended. Those figures have not been measured.

## From source (available now)

From the project folder:

```sh
cargo install --path crates/cli --locked
```

This installs `secbox` into `~/.cargo/bin`. If you use asdf, run `asdf reshim rust` afterwards. To install somewhere else, without touching your usual folders:

```sh
cargo install --path crates/cli --locked --root /tmp/secbox-install
export PATH="/tmp/secbox-install/bin:$PATH"
```

`up`, `down` and `status` also need the daemon, so install it too with `cargo install --path crates/daemon --locked`. Start it with `SECBOX_ENGINE_BIN=/absolute/path/to/msb secboxd` and leave it running.

Check the install:

```sh
secbox --version
secbox doctor
```

## Uninstalling

1. Delete secrets you stored, in each project: `secbox secrets list`, then `secbox secrets rm NAME`. Nothing else removes them from the keychain.
2. Remove the state folder: `secbox uninstall --purge`. It lists everything it would remove, and asks you to type the product name. `--yes` skips that question.
3. Remove the program: `cargo uninstall cli`, or delete the binaries.

`uninstall --purge` removes only `~/.secbox`, or `$SECBOX_HOME/.secbox` if that is set. It refuses if that path is a symbolic link or not a directory, if the folder holds other files and has no marker from this tool, or if a sandbox in it is still running. Engine data under `~/.secbox/msb` goes with it, including downloaded images.

## Homebrew through a tap (NOT AVAILABLE YET)

**Nothing has been released yet**, so these commands don't work today. They are what the first release will offer, on Apple Silicon Macs:

```sh
brew install --cask ketanmujumdar/tap/secbox-desktop   # the desktop app, which pulls in the formula
brew install ketanmujumdar/tap/secbox                   # or only the CLI and the daemon
brew services start secbox                              # run the daemon now and at login
```

The formula installs `secbox` and `secboxd`, and the engine pinned to the version secbox was tested on, under its own `libexec`. Nothing else on your `PATH` changes. To uninstall: `brew services stop secbox`, the steps in [Uninstalling](#uninstalling), then `brew uninstall --cask secbox-desktop` and `brew uninstall secbox`.

How releases are built, signed and published, and what has not been tested, is in [RELEASING.md](RELEASING.md). The templates are `packaging/homebrew/secbox.rb` and `packaging/homebrew/secbox-desktop.rb`.

`brew audit` does not accept a file path, so it can only check a formula inside a tap. It also turns on Homebrew's developer mode and installs its own gems the first time it runs. `brew developer off` turns the mode off again.

## homebrew-core (later)

Getting into the main Homebrew repository would let people run `brew install secbox`. Its [acceptable formulae](https://docs.brew.sh/Acceptable-Formulae) rules are the ones to check at that point. Expect to need:

- A stable, tagged release with a working source tarball.
- An open-source license. The project uses Apache-2.0.
- Evidence of real use. Homebrew's notability checks look at things like stars, forks and watchers, and it does not accept tools that are too new or too niche.
- No name clash. An openSUSE package is already called `secbox`, so settle the final name first.
- A meaningful `test do` block, and a passing `brew audit --new --formula secbox`.

Stay with the tap until the tool actually provides protection.
