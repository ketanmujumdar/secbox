# Uninstall

Removing secbox takes three steps: delete stored secrets, remove the state folder, then remove the programs. Do them in this order, because nothing else removes keychain entries.

## 1. Delete stored secrets

In each project where you stored secrets:

```sh
secbox secrets list
secbox secrets rm NAME
```

## 2. Stop sandboxes and remove the state folder

Stop or destroy any running sandbox first, with `secbox down --destroy` in each project. Then:

```sh
secbox uninstall --purge
```

It lists everything it would remove and asks you to type the product name. `--yes` skips the question. Without `--purge`, nothing is removed.

`uninstall --purge` removes only `~/.secbox`, or `$SECBOX_HOME/.secbox` if that is set. That includes the engine's data under `~/.secbox/msb`, such as downloaded images. It refuses if:

- the path is a symbolic link or not a folder
- the folder holds other files and has no marker showing secbox created it
- a sandbox in it is still running

## 3. Remove the programs

```sh
cargo uninstall cli
cargo uninstall daemon
```

Or delete `secbox` and `secboxd` from wherever you installed them. If you linked the desktop app's CLI into `/usr/local/bin` or `~/.local/bin`, remove that link too.
