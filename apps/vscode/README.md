# secbox sandbox

Lets VS Code edit the files of a running secbox sandbox. `secbox open --sandbox` installs this into a separate VS Code profile and opens the project in it. See `guide/src/guides/editors.md`.

Plain JavaScript with no dependencies and no build step. The CLI embeds these files, so there is nothing to publish or install by hand.

The rule this exists to keep: **nothing from the sandbox runs on this machine.** The files are never on disk here, `.vscode` and `.git` are hidden from VS Code, and Git goes through the daemon. `crates/cli/src/editor/sandbox.rs` enforces the hiding on the host side, so it does not depend on this code.
