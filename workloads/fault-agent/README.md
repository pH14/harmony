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

A window is identified by its target and its start, so two `RunHook` windows
for one hook that touch launch it twice even when no poll falls in between.

A node that exits while no fault names it is an unexpected death: the agent
counts it and starts the node again on the next tick.

## The bundle

The agent reads its workload description from a bundle file, one item per line:

```text
setup <argv...>          a command run once, before any node starts
node <name> <argv...>    a supervised long-lived process
hook <id> <argv...>      a one-shot command a RunHook fault launches
ready <argv...>          a command that exits 0 once setup is done
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
reported `assert_sometimes` ids, unexpected deaths, restarts, and parked
threads. The tick register is emitted every tick so liveness is always fresh;
the others are emitted only when they change.

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
