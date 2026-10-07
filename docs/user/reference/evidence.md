# Saved evidence

Harmony stores named searches and branches under `.harmony/runs`. You can inspect
a result by name or pass its saved directory directly. Each new result needs
an unused name or output directory so that it won’t overwrite earlier evidence.

Use `harmony list` to find saved results and `harmony show` to examine them. Both
provide JSON output for automation; the [command reference](cli.md) lists the
available options and selectors.

## Keep the whole result directory

A saved result includes a manifest with its workload, runner, execution
identities, artifact hashes, and relationship to its parent. The workload’s
records describe the actions and outcomes, while a search also retains its
exploration evidence and available checkpoints.

Branches can contain saved guest state. Interactive branches also retain a
terminal transcript, which helps explain the changes you made. Continuing a
search after those changes depends on the saved state, so keep the full result
directory rather than just copying the report or transcript.

Console output is kept as a bounded tail, and adjacent observations can overlap.
Use it with the timeline and assertions when investigating; it may not contain
everything the application printed. Deleting a result directory removes the
evidence stored there.

## Interpret exit status

| Status | Meaning |
| --- | --- |
| 0 | The operation completed successfully. |
| 1 | An application search found a violation or unmet reachability condition, or an application command failed. |
| 2 | Invalid input, an infrastructure failure, or a rejected reproduction. |

The counter tutorial expects its search to exit with status 1 because it finds
the intended bug. In automation, check the finding evidence as well as the exit
status so that an unrelated failure cannot stand in for the expected result.
