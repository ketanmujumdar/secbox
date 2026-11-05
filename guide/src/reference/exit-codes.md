# Exit codes

Every `secbox` command exits with one of these codes, so scripts and CI can tell outcomes apart.

| Code | Meaning |
| --- | --- |
| 0 | Success |
| 1 | A check failed, or something was refused or aborted |
| 2 | Bad usage, such as an unknown option or a missing argument |
| 3 | `check --strict` found Critical or High findings |
| 4 | `net check` explained a denied request |
| 5 | `diff` flagged something, or its review stopped at a limit |
| 69 | The command is not built yet (`export`) |

`exec` and `shell` are different: they exit with the exit code of the command or shell that ran inside the sandbox. After Ctrl-C, `logs -f` exits like any interrupted program, which most shells report as 130.

## Examples

```sh
secbox check --strict . ; echo $?                  # 0 or 3
secbox net check policy.yaml PUT https://registry.npmjs.org/x ; echo $?   # 4
secbox exec -- sh -c 'exit 7' ; echo $?            # 7
secbox diff ; echo $?                              # 0, or 5 if something was flagged
secbox export ; echo $?                            # 69
```
