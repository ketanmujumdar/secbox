# Releasing

A release is one tag. Pushing `vX.Y.Z` runs `.github/workflows/release.yml`, which builds on an Apple Silicon runner, signs with Developer ID, notarizes, publishes a GitHub pre-release and updates the Homebrew tap. **None of this has run yet.** The first release will be the first test of the workflow, the formula and the cask.

## What users get

```sh
brew install --cask ketanmujumdar/tap/secbox-desktop   # the app, and the formula below with it
brew install ketanmujumdar/tap/secbox                   # or only the CLI and daemon
brew services start secbox                              # the daemon, now and at login
```

| Piece | Where it comes from | Signing |
| --- | --- | --- |
| `secbox`, `secboxd` | `secbox-X.Y.Z-darwin-aarch64.tar.gz` on the GitHub release, installed by `Formula/secbox.rb` | Developer ID, hardened runtime, notarized |
| `msb` and `libkrunfw.5.dylib` (the engine) | microsandbox's own release, pinned to 0.7.13 by sha256 in the formula, under `libexec/engine` | microsandbox's ad-hoc signature with the hypervisor entitlement, untouched |
| `secbox.app` | `secbox-desktop-X.Y.Z-aarch64.dmg`, installed by `Casks/secbox-desktop.rb` | Developer ID, notarized and stapled, as is the `.dmg` |

The engine is pinned on purpose. Users run the engine version secbox was tested on (`TESTED_VERSION` in `crates/engine/src/msb.rs`), not the newest one. To move it, change both, run the real-VM checks in `docs/NEEDS_A_VM.md`, then update the `resource "engine"` URL and sha256 in `packaging/homebrew/secbox.rb`.

Formula binaries are not quarantined by Homebrew, so Gatekeeper doesn't check them and an ad-hoc signature would run. They are signed with Developer ID anyway, so that the keychain recognizes the daemon across upgrades and a copied binary still runs. The app does need notarization, or Gatekeeper blocks it.

## One-time setup

### 1. A Developer ID Application certificate

This is a different certificate from the one used for App Store apps.

1. In [Certificates, Identifiers & Profiles](https://developer.apple.com/account/resources/certificates/list), create a **Developer ID Application** certificate (the G2 Sub-CA) from a CSR made in Keychain Access.
2. Install it, then export it with its private key from Keychain Access as a `.p12` with a strong password.
3. Note its full name, as `security find-identity -v -p codesigning` prints it: `Developer ID Application: Your Name (TEAMID)`.

### 2. An App Store Connect API key, for notarization

In [App Store Connect, Users and Access, Integrations](https://appstoreconnect.apple.com/access/integrations/api), create a Team key with the **Developer** role. Download the `.p8` (it can be downloaded once) and note its Key ID and the Issuer ID.

### 3. The tap repository

Create a public repository `ketanmujumdar/homebrew-tap` with a README and empty `Formula/` and `Casks/` folders. Then create a fine-grained personal access token limited to that one repository, with **Contents: Read and write**.

### 4. Secrets in this repository

Settings, Secrets and variables, Actions. Put them in a `release` environment with a required reviewer if you want each release to wait for approval.

| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE` | `base64 -i cert.p12 \| pbcopy` |
| `APPLE_CERTIFICATE_PASSWORD` | The `.p12` password |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_API_KEY_ID` | The API key's Key ID |
| `APPLE_API_ISSUER` | The Issuer ID |
| `APPLE_API_KEY_P8` | The whole contents of the `.p8` file |
| `HOMEBREW_TAP_TOKEN` | The token from step 3. Without it the release is published and the tap step is skipped |

## Each release

1. Set the same version in `Cargo.toml` (`[workspace.package]`) and `apps/desktop/src-tauri/tauri.conf.json`. The workflow refuses a tag that matches neither.
2. Run `docs/MANUAL_TEST.md` and the checks in `docs/NEEDS_A_VM.md` that the release touches, on a real Mac.
3. Tag and push:

   ```sh
   git tag -s v0.1.0 -m "secbox 0.1.0"
   git push origin v0.1.0
   ```

4. When the workflow is green, on a clean Mac (or a new user account):

   ```sh
   brew update
   brew install --cask ketanmujumdar/tap/secbox-desktop
   brew test ketanmujumdar/tap/secbox
   brew audit --strict ketanmujumdar/tap/secbox
   brew services start secbox
   secbox doctor
   ```

If the tap step was skipped, render the files yourself and push them to the tap:

```sh
scripts/render-homebrew.sh 0.1.0 secbox-0.1.0-darwin-aarch64.tar.gz secbox-desktop-0.1.0-aarch64.dmg ../homebrew-tap
```

## Not tested yet

- The workflow has never run. The keychain import, Tauri's signing of the sidecars, and notarization of bare executables are the likely first failures.
- Hardened runtime on `secboxd` with Touch ID, the keychain and the SSH relay. If something breaks, it needs an entitlement, not a weaker signature.
- The daemon under launchd: that `HOME` and `SSH_AUTH_SOCK` reach it, and that `SECBOX_ENGINE_BIN` from the service block is used.
- The desktop app with the Homebrew daemon. The app brings its own `secboxd` and starts it when none answers. Started from Finder, that copy has no `SECBOX_ENGINE_BIN`, so it won't find the engine. Its *Restart daemon* stops whichever daemon is running, including the launchd one, which launchd then restarts. Start the daemon with `brew services` until the app learns where Homebrew put the engine.
- Linux and Intel Macs are not packaged. Build from source there (`docs/INSTALL.md`).
