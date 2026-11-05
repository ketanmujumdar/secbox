# secbox

secbox is being built to run a project and its AI agent inside a disposable microVM, so that a compromised dependency or a tricked agent stays away from your machine. `docs/ARCHITECTURE.md` has the design.

`secbox` is a working name. See [Changing the name](#changing-the-name).

## Status: early, and not protecting anything yet

**Do not rely on this tool to contain anything.** No command starts a VM, controls network traffic or reviews Git changes. The commands that would do so (`up`, `down`, `status`, `diff`, `export`) only say they are not built, and exit with code 69.

| Command | State |
| --- | --- |
| `doctor` | Works. Checks the host, without needing Docker |
| `check [path]` | Works. Reads a project's Compose file as data and reports what it asks for |
| `resources [path]` | Works. Shows requested and resolved limits, but nothing applies them yet |
| `secrets import/set/list/rm` | Works, with the OS keychain. Nothing delivers secrets to a VM yet |
| `net check <policy> <METHOD> <url>` | Works. Explains a policy decision, but nothing enforces it yet |
| `uninstall --purge` | Works. Removes the state folder only |
| `up`, `down`, `status`, `diff`, `export` | Not built |

## Quick start

You need a Rust toolchain, and macOS on Apple Silicon or Linux. There is no Homebrew tap or release yet, so [install from source](docs/INSTALL.md):

```sh
cargo install --path crates/cli --locked
secbox doctor
secbox check tests/fixtures/risky
secbox net check tests/fixtures/policy.yaml PUT https://registry.npmjs.org/left-pad
```

`docs/MANUAL_TEST.md` is a step-by-step script for trying everything that is built.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success |
| 1 | A check failed, or something was refused or aborted |
| 2 | Bad usage |
| 3 | `check --strict` found Critical or High findings |
| 4 | `net check` explained a denied request |
| 69 | The command is not built yet |

## Files it keeps

State lives in `~/.secbox`, owner-only: per-project secret names, their assignments and a names-only audit log. Secret values go only to macOS Keychain or Linux Secret Service. Set `SECBOX_HOME` to use `$SECBOX_HOME/.secbox` instead, which helps when testing or when your home path is too long for Unix sockets.

## Changing the name

The name is defined once, in `crates/brand/src/lib.rs`. Rust code outside that crate must use its constants, and a test in that crate fails if it does not.

```sh
scripts/rename.sh newname   # lowercase letters and digits, starting with a letter
cargo test --workspace
```

The script rewrites project text files and renames files that carry the name, such as the Homebrew formula. Some things it does not do:

- It does not rename the project folder.
- It does not change images in `docs/images`, which show the old name.
- It leaves lines about the unrelated openSUSE package alone. Review the "Final name" row in `docs/ARCHITECTURE.md` by hand.
- It does not move existing data. The old `~/.secbox` and old keychain entries stay under the old name, so remove them first with the old binary.

Avoid a new name that is a common word or a part of other identifiers. The name is replaced as plain text wherever it appears, and the guard test matches any text containing it.
