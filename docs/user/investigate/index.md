# Investigate a finding

Start with the evidence, select an earlier point, then change one thing you want
to understand. The examples here continue the [counter walkthrough](../start/first-bug.md)
and use its `baseline` search and `debugging` branch.

## Read the evidence

{{ example "inspect" }}

`show` summarizes a saved search or branch. Select a finding to examine one
execution. `--timeline` shows its actions and observations; `--logs` shows retained
console output. Logs are bounded, so absence from the retained tail is not proof
that a message was never printed.

## Choose a point

| Selector | Meaning |
| --- | --- |
| `--finding N` | Select the finding's execution. |
| `--step N` | Select the boundary after N recorded actions; step 0 is the prepared initial state. |
| `--rewind N` | Move back N actions from the selected failure observation or execution endpoint. |
| `--rewind-time 2s` | Move back in guest virtual time, rounded to an available boundary. |

A finding can first appear during recovery after the last disturbance. Its
failure point includes those recovery actions. Check the resolved point in the
branch output rather than assuming it is the last search action.

## Inspect or change the guest interactively

{{ example "shell" }}

Inside the guest shell, enter:

{{ example "shell-input" }}

The first command should print `investigating`, the marker saved by the earlier
branch. Exiting saves the resulting guest state as `interactive`, including the
new file. It does not automatically continue the old recorded suffix.

{{ example "shell-search" }}

This search starts from the saved state. It does not recreate your changes by
running a transcript of shell commands.

## Run a repeatable investigation script

Save the following as `inspect.sh` in the example directory:

{{ file "docs/examples/inspect.sh" }}

{{ example "script" }}

`--exec-file` executes that script inside the guest. `--exec` accepts a command
instead. Both normally continue the recorded suffix after the command; `--stop`
saves the changed point without continuing it. An interactive shell saves on exit.

Commands execute in the workload's environment. Its image must contain the
shell and utilities you use. The tutorial's prepared C image does; a minimal
`scratch` image may not.

## Compare what happened

{{ example "compare" }}

`diff` compares configuration, recorded inputs, and observed outcomes. Use it
to see what changed between experiments. It is not a complete guest filesystem
or memory diff and does not establish why an outcome changed. Host wall-clock
duration is excluded from the semantic comparison.

Changing the guest creates a new experiment. Rebuilding the application creates
a different executable identity. Neither is an exact reproduction of the
original execution. See [determinism](../concepts/determinism.md).
