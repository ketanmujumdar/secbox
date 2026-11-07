# Renaming the project

The name is defined once, in `crates/brand/src/lib.rs`. Rust code outside that crate must use its constants, and a test in that crate fails if it does not.

```sh
scripts/rename.sh newname   # lowercase letters and digits, starting with a letter
cargo test --workspace
```

The script rewrites project text files and renames files that carry the name, such as the Homebrew formula. Some things it does not do:

- It does not rename the project folder.
- It does not change images in `docs/images`, which show the old name.
- It rewrites the desktop app's `tauri.conf.json`, but no other JSON, HTML or JavaScript. The app's page asks the app for the name at run time, and the app takes it from the brand crate, so the page has none to change.
- It leaves lines about the unrelated openSUSE package alone. Review the "Final name" row in `docs/ARCHITECTURE.md` by hand.
- It does not move existing data. The old `~/.secbox` and old keychain entries stay under the old name, so remove them first with the old binary.

Avoid a new name that is a common word or a part of other identifiers. The name is replaced as plain text wherever it appears, and the guard test matches any text containing it.
