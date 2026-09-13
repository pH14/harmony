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
| `ProcPause` | `SIGSTOP` | `SIGCONT` |
| `ProcRestart` | `SIGKILL` | start the node again |
| `RunHook` | launch the hook once | nothing: hooks are not awaited |
| `ProcPark` | arm the park on the node's group | disarm it; a hold in progress finishes |
| `ProcEventKill` | arm the runtime event channel | disarm it; a reported fire kills that incarnation |
| `ProcEventPark` | arm the runtime event channel | disarm it; a reported fire holds the caller |

A window is identified by its target and its start, so two `RunHook` windows
for one hook that touch launch it twice even when no poll falls in between.

A node that exits while no fault names it is an unexpected death: the agent
counts it and starts the node again on the next tick.

An event-kill fire is credited only after a report matches the acknowledged
rarity and window-start identity for that node incarnation. A node death
without that report is an unexpected death, even while the event was armed. A
reported window is not rearmed after the node restarts; a new window start is a
new arm identity.
Event-park fire counts come from the runtime status reply. A failed event
transport while a command or arm is outstanding publishes
`fault_agent.infrastructure_error` and stops the agent so an injection cannot
be mistaken for a successful run.

## The bundle

The agent reads its workload description from a bundle file, one item per line:

```text
setup <argv...>          a command run once, before any node starts
node <name> <argv...>    a supervised long-lived process
hook <id> <argv...>      a one-shot command a RunHook fault launches
ready <argv...>          a command that exits 0 once setup is done
workload <argv...>       one long-lived workload command
check <argv...>          a short-lived oracle command run continuously
```

A node's id is its line order among `node` lines, from 0 — the same id the host
names in a `DecisionClass::Process` target, so the two sides agree without a
handshake. The image's init mounts `/proc`, `/sys` and `/dev` and nothing else,
so a workload that needs `/dev/shm`, a writable `/tmp`, or a configured loopback
interface asks for them on the `setup` line.

Validate a bundle on any host, including this development host:

```sh
cargo run --manifest-path workloads/fault-agent/Cargo.toml -- \
  --check-bundle --bundle path/to/bundle
```

The `ready` command is checked before setup completes. When it is configured,
every supervised node start begins a new recovery generation and launches the
command as a child probe. The standing-fault poll continues while the probe
runs; each tick checks whether it has exited and starts another attempt after a
failed attempt. A slow or hung probe therefore does not stop polling, and hooks
requested during recovery remain queued until the current generation is ready.
If no `ready` command is configured, recovery remains immediately ready and
hooks keep their existing behavior. Readiness comes from the command itself; the
agent does not require every node to be alive before a hook can run.

Queued hook requests preserve their order and repeated requests. The generation
only controls new hook launches: a hook that was already running can finish
after a later node start and continues publishing its directives and exit-42
failure. The hook owns the validity of that assertion, so the agent does not
discard an oracle result merely because another node recovered.
The hooks-started observation advances only after a queued or immediate request
successfully spawns its child; a request held behind readiness is not counted as
running.

After initial readiness, a `workload` command starts once and is left running;
it is not restarted when a node recovers. A `check` command starts after
readiness and runs serially: the next check starts as soon as the previous one
finishes, even when the node topology has not changed. Check output is forwarded
to the SDK as oracle directives, and the oracle owns the validity of those
assertions. The loop has no fixed service-specific interval and does not use a
global all-nodes-alive condition.

## Hook directives

A hook reports its own assertions by writing one directive per stdout line,
which the agent forwards to the SDK:

```text
@sometimes <u32>          assert_sometimes hit at that point
@reachable <u32>          assert_reachable at that point
@always <u32> <0|1>       assert_always(cond) at that point
```

Any other line is ordinary output. A hook that exits 42 reports a failed
assertion without writing a line. A line that starts with `@` but does not parse
is logged on serial rather than ignored, so a misspelled oracle line does not
read as a healthy run.

## Observations

The agent publishes IJON state registers the host reads back as SDK events:
completed ticks, the alive bitmap, hooks started and finished, the bitmap of
reported `assert_sometimes` ids, unexpected deaths, restarts, parked threads,
event-kill fires and sites, event-park fires, workload and check lifecycle, and
infrastructure errors. It also publishes a monotonic disturbance generation,
the automatic check-enabled mode, pending process faults and arms, and the
latest successful check's run number, generation range, and successful
`@reachable`/`@sometimes` point bitmap. The generation advances on observed
process transitions and valid runtime event reports, so a check that completed
before a later disturbance cannot serve as current evidence. The tick register
and pending-fault fence are emitted every tick; the other registers are emitted
when they change.

`pending_faults` counts active process windows and event arms that can still
produce a disturbance, together with queued or in-flight mutating commands, runtime
armed state, and an acknowledged EventKill awaiting the child death and
recovery transition. Pure park-status polls do not keep it pending, and a
disarm acknowledgement for an expired window clears the corresponding arm.
A canonical standing EventKill identity leaves the process-window count after
its matching report is consumed, even when its standing window continues.
Each poll publishes an invalid pending fence before mutations and publishes
the final pending count last, so a partial register snapshot cannot pass as
quiescent evidence.

`fault_agent.event_ready` is a per-node capability bitmap. An instrumented
runtime announces its protocol on the report channel before the agent sends any
event command; the bit is cleared for each new process incarnation until that
announcement arrives. Event requests seen before the announcement stay queued,
while a node without the runtime remains eligible for ordinary process faults.

The event runtime is the workload-owned C component in
[`runtime/`](runtime/README.md). Fault image recipes install its composed
`build/libvoidstar.so` at `/usr/lib/libvoidstar.so` for every instrumented
workload. The SDK shim only invokes an optional instrumentation hook; event
command and report semantics stay in this package.

## Boundaries

The portable library — bundle parsing, fault decoding, reconciliation, directive
parsing, register bookkeeping — builds and tests on any host. The binary adds
the Linux glue: the `/dev/harmony` ioctl transport, the `/dev/harmony-park`
ioctls, process spawning into per-node process groups, and process-group
signalling. Off x86-64 Linux only `--check-bundle` runs.

The standing poll rides the generic SDK opaque service request under
`fault_policy::STANDING_NAMESPACE`, with the poll tick as the request id and an
empty request body. The answer is decoded with `fault_policy::parse_standing`,
the same codec the host package encodes with.

Nothing in the agent reads a clock or an unseeded generator, no `HashMap` or
`HashSet` exists in the crate, and every collection it iterates is kept in a
canonical order, so two same-seed runs issue the same signals in the same order.

## Guest build

`build.sh` produces the fully static musl binary the guest image carries. It
must run on x86-64 Linux and prints the binary path on its last line.

```sh
./workloads/fault-agent/build.sh
```

A socket closure first remains pending until the next node-status reconciliation.
Final kill reports can still be drained during that interval. A reported kill
stays pending until the child is reaped; an unexplained closed transport on a
still-live instrumented node invalidates the execution. This distinguishes a
runtime exit between polling steps from a malformed protocol reply.

The Linux transport regressions run natively in the quality workflow. They are
ignored under Miri because the pinned interpreter does not support the socketpair
nonblocking ioctl. Descriptor inheritance still executes its actual duplication
and ownership path under Miri, alongside the portable library tests.
