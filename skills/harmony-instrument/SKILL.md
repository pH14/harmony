---
name: harmony-instrument
description: Get properties into a Harmony workload as checks that actually report. Use when writing the workload bundle, adding assertions or observations, or diagnosing why a check produced no telemetry. Covers the bundle format, hook stdout directives, assertion ids and their declared meanings, and confirming reports arrived rather than assuming they did.
---

# Instrumenting a workload

Harmony runs the whole application inside one deterministic VM. Instrumentation
means telling the guest-side fault agent what to supervise, what checkers to
run, and what each report means.

## The bundle is the contract

One file, `/etc/harmony/bundle`, inside the OCI image. The agent obeys it and
the host reads the same text to learn the action alphabet and the declared
meanings.

```text
setup <argv...>                    runs once, before any node starts
node <name> <argv...>              a supervised long-lived process
ready <argv...>                    must exit 0 before the run's setup is sealed
hook <id> <argv...>                a one-shot command the search may launch
describe node|hook <id> <text>     what that node or hook is
assert always|sometimes|reachable <id> [from <hook>] <text>
diagnostic <name> <argv...>        a command an investigator can run later
```

A node's id is its position among `node` lines from zero. Hook ids are yours to
choose. Quote arguments that contain spaces; the agent's tokenizer handles
`"..."` and `\"`.

Check a bundle on any host, including a development machine with no VM:

```bash
cargo run --manifest-path workloads/fault-agent/Cargo.toml -- --check-bundle --bundle path/to/bundle
```

## Checks report through hook stdout

A hook writes one directive per stdout line and the agent forwards it to the
SDK:

```text
@always <id> <0|1>     the always-claim at that id held (1) or failed (0)
@sometimes <id>        the sometimes-claim at that id was hit
@reachable <id>        that point was reached
```

Any other line is ordinary output, kept as console evidence. A hook that exits
42 reports a failed assertion without writing a line. A line starting with `@`
that does not parse is logged on serial rather than dropped, so a misspelled
directive does not read as a healthy run.

This is the supported route for an application Harmony cannot link a library
into. A guest program can also call the SDK ABI directly through
`/usr/lib/libvoidstar.so` (`fuzz_json_data`), but the workload's own OCI image
must carry that library — the fault package does not install it into the
workload rootfs, and no execution evidence for that route exists in this
repository. Use hooks unless you have qualified the library route yourself.

## Ids name properties; hooks name commands

Hook 3 is *the command that runs the checker*. Assertion 2 is *the claim the
checker evaluates*. They are different namespaces and are frequently confused.
Declare both, and connect them:

```text
hook 3 /usr/bin/pg_amcheck --heapallindexed -d faultlab
describe hook 3 pg_amcheck --heapallindexed
assert always 2 from 3 every required heap tuple has a matching index entry
```

`harmony -w W inspect bug-1` then reports the failed id, its meaning, and the
command that produced the verdict. Without the declarations it reports a bare
number, and whoever reads the finding has to guess.

Ids are permanent. Once a campaign has recorded a finding against id 2, that id
means what it meant then, forever.

## Setup, readiness, and what the guest actually has

The image's init mounts `/proc`, `/sys` and `/dev` and nothing else. A workload
needing `/dev/shm`, a writable `/tmp` beyond what init creates, or a configured
loopback interface asks for it on the `setup` line.

`ready` gates the sealed setup point. Every execution in the campaign starts
from that seal, so anything slow and deterministic — initializing a data
directory, loading a fixture — belongs before it, not in a node's startup path.
A `ready` command that returns too early makes every execution start from a
different amount of finished work.

## Observations the search consumes

The agent publishes state registers the host reads back: completed ticks, the
live-node bitmap, hooks started and finished, the bitmap of reported
`sometimes` ids, unexpected deaths, restarts, and parked threads. The faults
package pairs the `sometimes` set with the live-node bitmap and the
hook-completion count to decide whether an execution reached somewhere new.

So a `sometimes` id is not only documentation: it is the signal that steers the
search. A workload whose interesting situations have no `sometimes` claim gives
the search nothing to steer by.

Compiler coverage is a separate layer. Harmony implements the
`trace-pc-guard` callback ABI in `libvoidstar.so`, but the faults package's
search does not consume basic-block identities and does not place that runtime
in a workload rootfs. Do not report coverage-guided exploration you have not
demonstrated.

## Confirm the telemetry arrived

An instrumented workload that reports nothing looks exactly like a clean run.
Before trusting a campaign:

1. Run a short campaign and check `harmony -w W inspect` lists your declared
   properties.
2. Force a violation — a checker fixture that always reports `@always <id> 0` —
   and confirm the finding appears with your id.
3. Remove the checker and confirm the property shows as unevaluated rather than
   passing.

Step 3 is the one people skip. A workload where the hook never ran and a
workload where the hook ran and passed produce the same silence.

## What you leave behind

A bundle whose nodes start, whose `ready` gates a real setup point, whose
checks have declared ids and meanings, and three recorded runs showing a
positive result, a negative result, and a detected absence of telemetry.
