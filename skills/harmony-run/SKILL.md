---
name: harmony-run
description: Design a bounded Harmony campaign, investigate a finding through the CLI, and report what the evidence establishes. Use when running a search, explaining a bug Harmony found, or writing up results. Covers budgets, the workspace verbs, forking and running a branch, guest commands, citing evidence, and the limits of what a campaign proves.
---

# Running an experiment and investigating what it finds

A campaign explores executions of a prepared workload under faults. What it
leaves behind is a workspace: the findings, the checkpoints and evidence behind
them, and a journal of every change since.

## Design the campaign before running it

```bash
harmony search --package faults IMAGE.oci --out W --seed 7 --executions 100 --wall-minutes 5
```

`--seed` fixes the campaign's choices. `--executions` and `--actions` bound its
logical work; `--wall-minutes` bounds host time. `--horizon-ms` is the guest
time one fault action runs for, and `--ram-mib` the guest RAM. `--workers`
sets parallel evaluators.

Decide the budget first and write it down. Then say what the campaign is
looking for and what would count as evidence it looked. A campaign that ends
with no finding is a bounded search that did not find one — it is not proof
the application is correct, and it is not necessarily a bad integration. Check
your `sometimes` and `reachable` claims: if the interesting situation was never
reached, the campaign did not test what you meant.

Execution needs a Linux KVM host.

## Opening the workspace

`-w W` opens the directory. It does not attach to a running VM. Each command
opens the workspace, does bounded work, commits, and exits. Guest time is
frozen in between. Add `--json` for the same facts in a versioned document.

```bash
harmony -w W findings
harmony -w W inspect bug-1
```

`findings` lists each recorded failure, the properties it violated, what those
properties mean, and whether a replay reproduced it. `inspect` on a finding
adds the hook that produced the verdict, the inputs that preceded it, the
evidence retained at that point, the diagnostics the workload declares, and the
properties this point holds no verdict for.

Every reply ends with commands that are valid next steps. Use them; they carry
the ids you need.

## Rewind and run forward

```bash
harmony -w W fork bug-1 --rewind 3s --name trace
harmony -w W run trace --for 4s
harmony -w W run trace --until assertion:2:fail --within 4s
```

`fork` creates a continuation starting before the finding, inheriting the
source's remaining recorded inputs at their original virtual times. With no
intervention it reproduces the same failure at the same virtual moment with the
same state hash. A mismatch is a defect in Harmony, not a weaker replay to
accept.

`--for` advances guest time. `--until COND --within D` watches for a *new*
guest report matching the condition, bounded by `D` more virtual time — the
failure already in the history does not satisfy it. Reaching the bound reports
`stop: virtual_deadline` and `condition_met: false`. That is not a passing
property.

Neither flag is host waiting; `--wall-seconds` (default 30) is the separate
host watchdog. Advancement stops at the source continuation's recorded end
unless `--extend` allows running past it under its final environment.

## Run a command in the guest

```bash
harmony -w W exec trace --within 1s -- sh -c 'cat /run/amcheck.out'
harmony -w W exec --at bug-1 --within 1s -- sh -c 'ls /var/lib/postgresql/data'
```

Argv is delivered verbatim; a shell expression needs an explicit `sh -c`.
`--at` probes a point on an automatically named branch and leaves the source
where it is.

`exec` marks its branch **modified**, because command delivery is not part of
the reproducer record. Its checkpoint is retained, because the recorded inputs
cannot rebuild guest state a command changed. A modified branch cannot mint a
reproducer; the original finding stays recorded and reproducible. Descendants
stay modified.

Pass `--request-id ID` and a retry returns the committed result instead of
running the command twice. A command whose bound expires stays in flight: the
next `run` on that branch finishes it, and a second `exec` is refused until it
does.

Changing the application's own settings — raising a log level, enabling a
checker — is a legitimate use of `exec` and often the fastest route to
evidence. It also changes the execution. Confirm the change took effect, and
say in the report that the instrumented run is not the recorded one.

## Read the evidence

```bash
harmony -w W inspect trace@head console --since m-0007 --matches 'ERROR:'
harmony -w W inspect trace@head events
harmony -w W branches
```

Views are `console`, `events`, `command`, and `hash`. `--since MOMENT`,
`--limit`, `--offset`, and `--matches TEXT` bound and page them; the reply says
what it left out. `regs` and `read GPA LEN` are not implemented in this build.

Selectors: `bug-1` a finding, `trace@head` a branch's latest saved endpoint,
`trace@12.3s` an absolute virtual time in its history, `m-0007` an immutable
moment a previous reply returned. Moment ids keep naming the same point after
the branch advances, so cite them. A selector that resolves to nothing is
refused; a nearby point is never substituted.

Console lines carry only the interval they were drained over. SDK events carry
a stream position as well as a timestamp. The reply states which; do not report
a precision the capture does not have.

## Export and report

```bash
harmony -w W export bug-1 --out shared --evidence
```

`export` writes the original recorded reproducer. `--evidence` adds the
investigation evidence in its own directory with its own provenance, so a
reader can tell the recorded failure from what you did while looking at it.

Write the report from the artifacts:

- what ran: image identity, seed, budgets, and what actually stopped each run,
- what was found: the property, its meaning, the moment, and whether a replay
  reproduced it,
- what the evidence shows, citing moment ids and evidence ids,
- what you changed to get the evidence, and whether it changed the outcome,
- what you could not observe, and which properties reached no verdict.

State the verification scope: replay from boot on a fresh session establishes
something a continuation from a modified checkpoint does not. Do not offer a
root cause the evidence does not support. "The checker failed here, and here is
the guest state at that moment" is a complete result; a mechanism you inferred
without observing it is not.
