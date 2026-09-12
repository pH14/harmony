# Bug investigation: implementation plan

This is the investigation contract within the consolidated
[CLI and agent UX effort](CLI-AGENT-UX-PLAN.md). The consolidated plan owns
delivery order, preparation skills, and evaluations of the complete workflow.

## What we're solving

Harmony finds a failing execution, but understanding it still requires manual
debugging. Add a shell interface an inexpensive LLM agent can drive one command
at a time: inspect a finding, rewind, enable guest logging, run forward, and
read the evidence. Humans use the same interface.

Search leaves a workspace directory containing recorded inputs, findings,
snapshots, branches, and evidence. Investigation opens that directory directly.
Each command restores the required state, does its work, saves the result, and
exits. Guest time is frozen between calls. Start with the faults package and
the PostgreSQL concurrent-index workload.

Determinism is guaranteed. A fork without intervention must reproduce the same
failure at the same virtual moment with the same modeled-state hash and guest
events. A mismatch is a defect, never an acceptable weaker replay.

## Essential contracts

- A **moment** is an immutable point in an execution. A **branch** is a named
  continuation whose head advances. A **finding** names a failure and its
  evidence. A **probe** is an automatically named branch for a one-off command.
- `fork` restores verbatim and inherits the remaining recorded inputs at their
  original virtual times. Later input transitions apply to the branch's current
  state; restoring source snapshots would erase the investigator's changes.
- `run` and `exec` use the same advancement machinery. Faults and guest service
  requests continue to be handled while a shell command runs.
- `exec` marks its branch modified because command delivery is outside the
  reproducer record. This does not mean nondeterministic. Save a post-exec
  checkpoint and retain it: earlier recorded inputs cannot reconstruct it.
  Descendants remain modified; the original finding remains reproducible.
- Commit the checkpoint, evidence, and branch head together. A crash leaves the
  previous committed head. A caller-supplied request ID makes retries return a
  committed result; never silently reinject an interrupted command.
- Inspection preserves its source. Existing logs/events require no VM. Report
  actual capture precision: console lines may have only an interval, and SDK
  events need their stream position as well as their timestamp.

Reuse existing snapshot formats and package input semantics. Choose simple
versioned persistence; this plan does not prescribe a database or file layout.
Retain lightweight search history and snapshots needed for findings and branches,
rather than every search snapshot.

## Commands

These examples use `pg` as the workspace directory created by search, `bug-1`
as a finding returned by search, and `trace` as a branch name chosen by the user.
`-w pg` opens that workspace; it does not select a running VM session.

| Command | What it does |
| --- | --- |
| `harmony search --package faults pgcic-14.3.oci --out pg --stop-on bug` | Search the workload, save its history in `pg`, and stop after finding a bug. |
| `harmony -w pg findings` | List findings, their assertion IDs, and verification results. |
| `harmony -w pg branches` | List named branches and their saved endpoints. |
| `harmony -w pg inspect` | Describe the workspace and available diagnostics. |
| `harmony -w pg inspect bug-1` | Show this finding's failed assertion, preceding inputs, and evidence. |
| `harmony -w pg fork bug-1 --rewind 3s --name trace` | Create branch `trace` three virtual seconds before the finding, with the source's remaining inputs. |
| `harmony -w pg run trace --for 4s` | Resume the VM from `trace`'s saved endpoint for up to four more virtual seconds, then save its new endpoint. |
| `harmony -w pg run trace --until assertion:2:fail --within 4s` | Resume that branch until assertion 2 fails again, bounded by four more virtual seconds. |
| `harmony -w pg exec trace --within 1s -- sh -c 'echo diagnostic'` | Execute a guest shell command on `trace`, capture output, and save the resulting branch state. |
| `harmony -w pg exec --at bug-1 -- sh -c 'cat /run/amcheck.*.out'` | Create a probe from the finding, read the guest diagnostic files, and retain the output without moving the original finding. |
| `harmony -w pg inspect trace@head console` | Read the branch's captured serial output without advancing it. |
| `harmony -w pg export bug-1 --out shared-bug --evidence` | Export the original reproducer and associated investigation evidence. |

**Where assertions come from.** The workload or its checking agent reports
assertions through Harmony's guest SDK. IDs identify those checks; Harmony does
not invent an assertion from an error-looking log line. In this PostgreSQL
workload, hook 3 runs `pg_amcheck --heapallindexed`. When it detects a missing
index entry, the hook reports `@always 2 0`, which the fault agent forwards as
failed assertion 2. PostgreSQL itself does not know about this Harmony ID.

Search names the occurrence `bug-1`; `2` identifies the check that failed.
`inspect bug-1` must supply that ID and its meaning so the agent can construct
the next command. For example (illustrative output):

```text
finding: bug-1
assertion: 2
meaning: every required heap tuple has a matching index entry
reported by: hook 3 (pg_amcheck)
result: failed; checker reached a verdict
moment: m-failure
```

`--until assertion:2:fail` watches for a new failed evaluation of that check
after the run begins. It does not stop merely because the old failure is already
in the history. A crash or another assertion failure can stop the run earlier;
the reply names what actually stopped it.

**What the time bounds mean.** `--for 4s` asks the VM to advance four virtual
seconds, subject to earlier stops. `--within 4s` puts a maximum on waiting for a
condition or command completion. If the branch starts at virtual time 10s, that
bound is 14s. Reaching it without the requested assertion returns
`stopped: virtual_deadline; condition_met: false`, not a passing oracle verdict.
Neither flag means four seconds of host waiting.

For `exec`, `--within 200ms` allows the guest command up to 200 virtual
milliseconds to complete; it stops earlier when completion is observed. The
default exec bound is 1s. Advancing commands also have a separate
`--wall-seconds` watchdog (default 30s) so a stuck VM cannot hang the CLI.
Both `run` and `exec` advance guest execution and deliver inherited inputs.

By default they stop at the source continuation's recorded end. `--extend`
explicitly allows running beyond that point under the final environment,
without generating new search actions. Logging may delay a failure, so the
worked session uses this option.

`trace@head` means the branch's latest saved endpoint. `trace@12.3s` addresses
an absolute virtual time in its history. Returned `m-...` IDs name immutable
moments even after the branch advances. Use those IDs to cite evidence or keep
a fixed starting point for a log query. Besides `console`, inspection supports
`events` (guest SDK reports), `regs` (CPU registers), `read GPA LEN` (guest
physical memory bytes), and `hash` (modeled-state hash).

Console/event inspection supports `--since SEL`, bounded `--limit N`, and
pagination; console supports `--match REGEX`. Resolve selectors once. Never
silently substitute a later state for an unavailable moment.

Text and `--json` report the same facts: moment, branch, virtual time,
recorded/modified history, applicable verification result, stop reason, and
evidence references. Distinguish guest failures from tool errors, incomplete
commands, and unevaluated oracles. Report truncation. Command completion alone
does not establish guest exit status.

Preserve argv through guest-shell quoting; shell expressions require explicit
`sh -c`. A timed-out command may still be running: retain its pending capture
state and allow `run` to finish it before accepting another `exec`.

Keep existing bug JSON and replay compatibility. Export preserves the original
recorded reproducer; optional investigation evidence carries its own provenance.
No daemon, causal-analysis engine, or new fault-editing interface is needed.

## Implementation order

Read the code named for the current step and follow definitions as needed.

1. **Prove cold continuation first.** Start in
   `workloads/faults/src/consonance.rs`. Export a real pre-failure endpoint,
   exit, restore in another process, and continue without intervention.
   Compare source hashes, moments, and events, including across an action
   boundary and from inside an active window. Splitting a run across commands
   must also preserve the result.

   Existing traps: `ensure_prefix` installs each prefix's environment; flattening
   the whole future schedule changes execution. Protocol `Replay` restores
   verbatim, while `Branch` installs an environment and reseeds. Preserve the
   source's later transitions without adding a reseed at CLI fork. Also,
   `FaultTarget::portable_snapshot()` currently exports setup; use the actual
   endpoint. Reuse the client's portable/sparse snapshot APIs.

2. **Connect search and the CLI.** Publish workspace findings from
   `workloads/faults/src/package.rs`; add CLI dispatch in `cli/src/main.rs`.
   Pin artifact identities, inputs, continuation position, source evidence,
   and required checkpoints. Implement listing, summary inspection, fork,
   and bounded run. Persist original hashes needed for exact verification:
   today's oracle confirmation alone is insufficient. Keep fault semantics
   in the package and CLI presentation in the CLI.

3. **Make persistent exec work.** The fault guest preparation currently starts
   the agent without a diagnostic serial shell; provide one in the workload's
   filesystem context during preparation. The current `ControlServer::exec`
   bypasses scheduling and rejects service questions. Refactor it to share
   normal advancement, reusing its serial capture parser. Implement persistent
   exec first, then reuse that path for `exec --at`.

   Test an input boundary and a service request during exec, a logging change
   surviving cold continuation, a nonzero command, timeout, output truncation,
   and interrupted/retried operations. Verify modified histories cannot mint
   reproducers and their original sources remain unchanged.

4. **Finish evidence and acceptance.** Expose workload-declared hook/assertion
   meanings, connection arguments, log destinations, and diagnostic tools in
   finding summaries. Include preceding inputs and concrete next commands.
   Add remaining inspection views and export. Update the CLI README with the
   actual workflow. Run owning-crate checks and the real VM integration tests;
   portable tests alone do not establish this feature.

## Worked workflow and acceptance

With pinned guest artifacts configured, the intended PostgreSQL session is:

```sh
harmony search --package faults pgcic-14.3.oci --out pg --seed 7 --stop-on bug
harmony -w pg inspect bug-1
harmony -w pg fork bug-1 --rewind 3s --name trace
```

The fork reply identifies the branch and its starting moment, for example:

```text
branch: trace
moment: m-fork
history: recorded
continuation: inherited from bug-1's source execution
```

Now `exec trace` operates on that branch. The following `run trace` restores
the state saved by exec, including the changed logging settings:

```sh
harmony -w pg exec trace --within 200ms -- \
  /usr/lib/postgresql/bin/psql -X -v ON_ERROR_STOP=1 \
  -h /tmp -U postgres -d faultlab \
  -c "ALTER SYSTEM SET log_min_messages = 'debug5';" \
  -c "ALTER SYSTEM SET log_statement = 'all';" \
  -c "SELECT pg_reload_conf();"
harmony -w pg run trace --until assertion:2:fail --within 4s --extend
harmony -w pg inspect trace@head console --since m-fork --match 'LOG:|ERROR:'
harmony -w pg exec --at bug-1 --within 1s --extend -- sh -c 'cat /run/amcheck.*.out'
```

Each invocation is a separate call. Replace `m-fork` with the returned immutable
fork moment and choose an available rewind point. Confirm logging actually took
effect. Report the instrumented outcome honestly; logging can change it.

Give an inexpensive model the CLI README and workload with: “Find a bug and
explain it with guest evidence.” It passes when it preserves the original finding,
obtains and cites evidence through these commands, identifies supported interacting
inputs, and states what it could not observe. Do not supply the historical
root-cause explanation or a solution recipe. Run a stronger model as a control.

Also test a logging-sensitive failure and a process restart between commands.
Hand off the actual command transcript, exact replay comparison, and evidence.
Mark unavailable hardware tests unrun; do not weaken their assertions.
