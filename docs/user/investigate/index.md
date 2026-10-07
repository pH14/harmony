# Investigate a finding

A finding gives you an execution to work from. You can examine its timeline,
return to an earlier point, and try a change to understand the failure.
The examples below continue the [counter tutorial](../start/first-bug.md), using
its `baseline` search and `debugging` branch.

## Read the evidence

Open the finding’s timeline and application logs:

{{ example "inspect" }}

`show` summarizes a saved search or branch; selecting a finding narrows it to one
execution. Use `--timeline` for recorded actions and observations, or `--logs`
for console output. Harmony retains a bounded tail of the logs, so older
messages may no longer be available.

## Choose where to branch

These selectors let you choose an execution and a point within it:

| Selector | Where it takes you |
| --- | --- |
| `--finding N` | The execution for finding N. |
| `--step N` | The boundary after N recorded actions. Step 0 is the prepared initial state. |
| `--rewind N` | N actions before the selected failure observation or execution endpoint. |
| `--rewind-time 2s` | An earlier point in guest virtual time, rounded to an available boundary. |

Some failures first appear during recovery after the last disturbance. Harmony
includes those recovery actions when locating the failure, so check the point
reported in the branch output to see exactly where you’ve landed.

## Open a shell

Start an interactive branch from `debugging`:

{{ example "shell" }}

In the guest shell, read the file from the earlier branch, inspect the counter,
and create another file:

{{ example "shell-input" }}

The first command should print `investigating`. When you exit, Harmony saves the
guest state as `interactive`, including the new file, without running the rest
of the original execution’s recorded actions.

You can now search from that state:

{{ example "shell-search" }}

Harmony restores the saved guest state for this search. It doesn’t need to rerun
your shell commands to recreate the changes.

## Run a script

For an investigation you want to repeat, save this as `inspect.sh` in the example
directory:

{{ file "docs/examples/inspect.sh" }}

Run it inside the guest with `--exec-file`:

{{ example "script" }}

You can also pass a command directly with `--exec`. Both forms normally continue
the remaining recorded actions after the command finishes. Add `--stop`, as
above, to save the changed state immediately. An interactive shell saves when
you exit.

The image must contain the shell and utilities your commands use. The tutorial’s
C image includes them; a minimal `scratch` image may not.

## Compare the results

Compare the original search with its follow-up, then list the saved results:

{{ example "compare" }}

`diff` shows changes in configuration, recorded inputs, and observed outcomes.
It excludes host wall-clock duration from that comparison. Use it to see how
two experiments differ, alongside their timelines and assertions; it doesn’t
compare every byte of guest memory or the filesystem, or determine the cause
of a changed outcome.

Commands that change the guest create a new experiment. Rebuilding the program
also changes its executable identity. See
[Determinism and investigation](../concepts/determinism.md) for how these changes
affect reproduction.
