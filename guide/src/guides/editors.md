# Editors

Your editor runs on your machine and works on your normal checkout. A hostile project can try to run code the moment you open it in an editor, through automatic tasks or agent hooks. `secbox open` checks for that first, then opens the folder.

```sh
secbox open                    # check, then open the current folder in VS Code
secbox open ~/code/myapp --editor cursor
secbox open --dry-run          # check and say what would start
```

## Supported editors

| `--editor` | Program it starts |
| --- | --- |
| `vscode` (default) | `code` |
| `vscode-insiders` | `code-insiders` |
| `cursor` | `cursor` |

The program is found on your `PATH`, ignoring relative entries. If it is somewhere else, set `SECBOX_EDITOR_BIN` to its absolute path. A project cannot set this. The folder is passed as one argument, with no shell, so nothing in a folder name is ever interpreted.

## The checks

```sh
secbox checks list
```

```text
Editor checks
  on   vscode-automatic-tasks           VS Code does not run automatic tasks
  on   vscode-insiders-automatic-tasks  VS Code Insiders does not run automatic tasks
  on   cursor-automatic-tasks           Cursor does not run automatic tasks
  on   vscode-workspace-trust           VS Code keeps Workspace Trust on
  on   vscode-insiders-workspace-trust  VS Code Insiders keeps Workspace Trust on
  on   cursor-workspace-trust           Cursor keeps Workspace Trust on
  on   project-folder-open-tasks        No project task asks to run when the folder opens
  on   project-settings-override        Project settings do not ask for these safeties to be off
  on   project-agent-hooks              No agent hooks are set in the project
```

| Check | What it looks at |
| --- | --- |
| `*-automatic-tasks` | Your user setting `task.allowAutomaticTasks` |
| `*-workspace-trust` | Your user setting that keeps Workspace Trust on |
| `project-folder-open-tasks` | Tasks in the project with `runOn: folderOpen` |
| `project-settings-override` | Project settings that try to turn these safeties off |
| `project-agent-hooks` | Hooks in coding-agent settings, such as `.claude/settings.json` |

Each result is `pass`, `fail` or `n/a` (for example, when that editor is not installed). The same checks run as part of `secbox check`.

## Turning a check off or on

```sh
secbox checks off cursor-workspace-trust
secbox checks on cursor-workspace-trust
```

These choices are stored in your state folder and are **never read from a project**, so a hostile `secbox.yaml` cannot switch a check off.

## When a check fails

`secbox open` refuses to start the editor. You can:

- fix your user settings by hand, or
- run `secbox check --fix`, which offers to change your **user** settings, asks first and keeps a backup. It never changes project files, or
- open anyway with `secbox open --force`. This is recorded in the audit log.

## Limits

This makes the safe path easier. It is not a security boundary: opening the folder another way skips the check.

There is no editor connection into the VM, and no SSH. Your editor's terminal runs on your machine, so run project commands with `secbox exec`.
