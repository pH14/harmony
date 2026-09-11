---
name: harmony-run
description: Run Harmony searches and durable finding investigations with accurate replay, branch, probe, command, and evidence semantics.
---

# Harmony runs

Use this skill when invoking Harmony, replaying a campaign, or investigating a
retained finding. Confirm the package, backend, input, artifact identities,
host support, and output directory first. `--seed` selects the deterministic
random stream; it does not bound work. `--executions` and `--actions` bound
logical campaign work, `--workers` controls parallel evaluators,
`--wall-minutes` bounds host time, and package options such as
`--horizon-ms` and `--ram-mib` describe guest execution. `--replay INPUT.json
--repeat N` replays recorded actions.

For a faults workspace, use the durable investigation commands:

```text
harmony -w W findings
harmony -w W branches
harmony -w W inspect FINDING
harmony -w W fork FINDING --rewind 3s --name BRANCH
harmony -w W run BRANCH --for 4s
harmony -w W exec BRANCH --within 1s -- ARGV...
harmony -w W exec --at SELECTOR --within 1s -- ARGV...
harmony -w W inspect BRANCH@head console
harmony -w W inspect BRANCH@head events
harmony -w W inspect BRANCH@head command
harmony -w W export FINDING --out DEST --evidence
```

`--for` and `--within` are guest virtual-time bounds; `--wall-seconds` is the
host bound for advancing work. `findings`, `branches`, `inspect`, and
`export` read retained history, while advancing a continuation requires the
supported Linux KVM path. Preserve exact argv words. Shell syntax requires an
explicit `sh -c` in the argv after `--`.

`fork` creates a named continuation and `run` advances and saves that branch.
`exec BRANCH` changes the branch head and retains command status, output, and
the new checkpoint. `exec --at SELECTOR` creates an automatically named probe
and leaves the selected source point unchanged. If a command is pending when
its bound ends, resume it with `run`; start another command only after it
completes.

Use `--json` for machine-readable output; virtual-time fields remain numeric
nanoseconds and also have human-readable companions. `--request-id` on
`fork`, `run`, or `exec` makes a retry return the already committed result
before guest artifact preparation or execution. Reuse an ID only for the same
operation and semantic arguments; a changed request is an error and must not
mutate the workspace. A retry is a durable lookup, not a second guest run.

After a mutation, inspect the saved moment, stop reason, state hash, and
evidence. Compare a probe with its source explicitly, and keep unconfirmed,
unevaluated, and execution-failure outcomes distinct from a reproduced
property violation.
