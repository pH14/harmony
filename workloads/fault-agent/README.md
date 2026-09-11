<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# fault-agent

The fault agent is the guest half of the standing-fault mechanism. It supervises
a workload's processes inside a Consonance guest, polls the host once per tick
for the faults whose V-time window contains the current moment, and applies the
difference against the previous answer as signals, hook launches, and parks.

The host answers with a state, not a command stream, so the agent turns state
into edges by diffing consecutive answers. A fault applies once when its window
opens and is undone once when it closes. A missed tick costs resolution and
nothing else.

| fault | window opens | window closes |
|---|---|---|
| `ProcKill` | `SIGKILL` the node's group | nothing: a kill is permanent |
| `ProcEventKill` | arm the instrumented runtime to synchronously kill at an event ordinal | disarm the runtime arm |
| `ProcPause` | `SIGSTOP` | `SIGCONT` |
| `ProcRestart` | `SIGKILL` | start the node again |
| `RunHook` | launch the hook once | nothing: hooks are not awaited |
| `ProcPark` | arm the park on the node's group | disarm it; a hold in progress finishes |
| `ProcEventPark` | arm the instrumented runtime to hold one thread at a rarely visited site | read back how many parks held, then disarm the runtime arm |

A window is identified by its target and its start, so two `RunHook` windows
for one hook that touch launch it twice even when no poll falls in between.

A node exit that is not marked expected by Kill or Restart is an unexpected
death: the agent counts it and starts the node again on the next tick. A death
observed with an EventKill arm installed also increments the dedicated
EventKill-fired counter; Kill and Restart windows mark their deaths expected
and do not increment either event outcome.
Each instrumented start also receives an internal, monotonically increasing
process incarnation id. The automatic coverage stream uses that id instead of
the recyclable Linux PID; it is guest state, so snapshots and replay reproduce
the same allocation without an operator setting.
When an image declares instrumented events, the same compatibility runtime is
injected automatically into nodes, hooks, and readiness probes. Instrumented
helper binaries therefore retain software exits without bundle-specific launch
commands.

## The bundle

The agent reads its workload description from a bundle file, one item per line:

```text
setup <argv...>          a command run once, before any node starts
node <name> <argv...>    a supervised long-lived process
hook <id> <argv...>      a one-shot command a RunHook fault launches
ready <argv...>          a command that exits 0 once setup is done
workload <argv...>       a long-lived process started once, after ready
check <argv...>          a command rerun after every node death or restart
```

The `workload` process is the load the faults act on and the `check` command is
its oracle. Both run on the agent's own schedule, so a search spends its drawn
actions on faults rather than on starting load and asking for verdicts. The
agent never restarts a `workload`; it counts the exit instead, because a run
whose load stopped early carries weaker evidence than one whose load ran
throughout.

A node's id is its line order among `node` lines, from 0 — the same id the host
names in a `DecisionClass::Process` target, so the two sides agree without a
handshake. The image's init mounts `/proc`, `/sys`, and `/dev`, configures the
private guest loopback interface, and gives the workload writable tmpfs mounts
at `/tmp` and `/run`. A workload that needs another pseudo-filesystem still asks
for it on the `setup` line.

Validate a bundle on any host, including this development host:

```sh
cargo run --manifest-path workloads/fault-agent/Cargo.toml -- \
  --check-bundle --bundle path/to/bundle
```

The readiness command is enforced before the setup point is sealed and after
every supervised node start. Recovery probes run as child processes and are
sampled at poll ticks, so a slow or hung probe never stops standing-fault
polling. Hook actions requested during recovery stay queued until the newest
node generation passes readiness. Output from a hook launched before a later
restart is drained but cannot publish assertions for the new generation. If
recovery never completes, those hooks remain unfinished and the endpoint is
inconclusive.

## Hook directives

A hook reports its own assertions by writing one directive per stdout line,
which the agent forwards to the SDK:

```text
@sometimes <u32>          assert_sometimes hit at that point
@reachable <u32>          assert_reachable at that point
@always <u32> <0|1>       assert_always(cond) at that point
@verified <u64>           workload units this hook has verified so far
```

`@verified` is a count, not an assertion. It is cumulative and monotonic, the
agent keeps the greatest value any hook has reported, and it publishes it in a
register rather than to the SDK. A check that emits only `@verified` reached no
verdict.

The `check` command reports through the same directives and the same exit code.
It is not held behind the post-restart readiness wait that hooks are: a check is
responsible for deciding for itself whether it could read the workload at all.
One starts whenever a node has died or restarted since the last one started,
which is when the verdict can change, and otherwise on a heartbeat that follows
what the last check cost. A check that asserted something read the workload back
through its own client and competed with it for the guest's single processor, so
the next one waits: a faster heartbeat spends the run on validation rather than
on load, and past a few tens of guest seconds it takes enough of the processor
that the workload stops making progress. A check that asserted nothing found no
workload to compare and returned without reading a node, so the next one follows
soon. A run that never reaches a first comparison produces no evidence at all,
and the runs a search draws are short.

Any other line is ordinary output. A hook that exits 42 reports a failed
assertion without writing a line. A line that starts with `@` but does not parse
is logged on serial rather than ignored, so a misspelled oracle line does not
read as a healthy run.

## Observations

The agent publishes IJON state registers the host reads back as SDK events:
completed ticks, the alive bitmap, hook actions accepted and finished, the bitmap of
reported `assert_sometimes` ids, unexpected deaths, restarts, parked
threads, EventKill-fired deaths, and workload exits. The tick register is emitted every tick so
liveness is always fresh; the others are emitted only when they change. The
EventKill-fired register is monotonic and is separate from unexpected deaths so
the host can identify the outcome of an EventKill action without inferring it
from aggregate process exits.

## Boundaries

The portable library — bundle parsing, fault decoding, reconciliation, directive
parsing, register bookkeeping — builds and tests on any host. The binary adds
the Linux glue: the `/dev/harmony` ioctl transport, process spawning into
per-node process groups, and process-group signalling. The `/dev/harmony-park`
ioctls are available only in the pre-existing x86 faultlab image; the arm64
faultlab image intentionally has no task-park device. On non-Linux hosts only
`--check-bundle` runs.

The standing poll rides the generic SDK opaque service request under
`fault_policy::STANDING_NAMESPACE`, with the poll tick as the request id and an
empty request body. The answer is decoded with `fault_policy::parse_standing`,
the same codec the host package encodes with.

Nothing in the agent reads a clock or an unseeded generator, no `HashMap` or
`HashSet` exists in the crate, and every collection it iterates is kept in a
canonical order, so two same-seed runs issue the same signals in the same order.

## Guest build

`build.sh` produces the fully static musl binary the guest image carries. It
must run on native Linux and selects the matching x86-64 or arm64 musl target
from the host architecture. It prints the binary path on its last line.

```sh
./workloads/fault-agent/build.sh
```
