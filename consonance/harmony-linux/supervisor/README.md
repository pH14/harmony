# Harmony supervisor

`harmony-supervisor` is the platform-owned child-process supervisor installed
inside the OCI rootfs at `/usr/lib/harmony/supervisor`. It reads the one
canonical execution document from `/run/harmony/execution.json`. The launcher
does not accept command-line policy: a missing bundle runs the document's
command once, while a bundle path selects the same binary's structured node,
hook, readiness, and standing-window path.

Before starting children, the supervisor creates a delegated child below the
device-policy cgroup, enters a cgroup namespace rooted there, and replaces the
mounted view. It moves into the delegated cgroup's `runtime` leaf and enables
available controllers at its empty namespace root.
The writable cgroup mount exposes only this container's subtree, so nested
runtimes can create sibling cgroups without managing the outer guest hierarchy.

The supervisor owns its child process groups and reaps descendants in the PID
namespace. Every child receives the resolved uid, gid, and supplemental groups
from the execution document. The supervisor retains its own privilege for
platform device operations. `/dev/harmony` is accessed through the shared
`hypercall-doorbell::linux::DeviceTransport`; the supervisor has no raw MMIO
transport. Device errors fail execution rather than silently omitting a
requested process action.

An application exit marker is emitted only after the plain execution command has
started and returned. Malformed execution input, structured setup errors, and
spawn failures emit `HARMONY_OCI_SUPERVISOR_FAILURE` with the preserved
errno-derived code, so a failed spawn with code 127 remains distinct from an
application that actually exits 127.

Tracked node and hook children retain their exit status until their owning
`Child` consumes it. Orphan cleanup enumerates the supervising thread's other
children and reaps them individually, so a finished hook, workload, check, or
readiness probe cannot lose its status to the node reaper. The canonical
kernels require `CONFIG_PROC_CHILDREN` for this enumeration.

The `ready` command is checked before setup completes. After setup, every
supervised node start begins a recovery generation and launches the command as
an asynchronous child probe. Standing-window polling continues while the probe
runs, and a failed attempt is retried on the next tick. New hook requests remain
queued in request order until the current generation becomes ready; hooks that
were already running retain their results. The hooks-started register advances
only after a queued or immediate request successfully spawns. A bundle without
a readiness command launches hooks immediately.

Every child the supervisor starts receives `ANTITHESIS_OUTPUT_DIR=/run/antithesis`.
The supervisor links `/run/antithesis/sdk.jsonl` to `/dev/harmony` at startup
and again after the setup command, because setup may mount a fresh `/run`, so a
process that follows the Antithesis fallback SDK writes each JSON record
straight to the host. The driver attributes each record to its writer's process
id in the writer's PID namespace, which the supervisor shares with the children
it starts. Before launching nodes, the supervisor opens its own assertion sink
through that link and retains the descriptor. A child that later replaces the
link or its containing mount cannot divert the built-in node-exit assertion.
Child stdout is discarded; assertions travel only as SDK records.

The supervisor declares one built-in check, the Always assertion
`workload node ends only by a fault the search injected`. A node that dies
while no kill, restart or event kill targets it, by a signal its own execution
raises (`SIGSEGV`, `SIGBUS`, `SIGABRT`, `SIGILL`, `SIGFPE`, `SIGTRAP` or
`SIGSYS`) or a nonzero exit status, violates it; the record's details name the
node and the exit. A death while a node start waits for the ready probe is
exempt only while recovery from an injected kill, restart, or event kill is
still pending. A later natural restart keeps that exemption until the current
readiness probe passes. A natural restart without pending fault recovery still
runs the ready probe and queues hooks, but a later independent crash remains a
violation.
The supervisor also checks the final child status after signaling a scheduled
kill or replacing a child. Its injected kills use `SIGKILL`, so a fatal signal
or nonzero exit status proves the child ended independently before the kill
took effect, even when that exit is reaped after the kill decision.

The exemption above lets a restarted node fail while the system is still
recovering, so a node that never recovers would go unreported. A second
built-in Always assertion covers that case:
`workload becomes ready within 30 seconds after faults end`. Once no fault
window is active and no injected fault is pending for 30 seconds of virtual
time, a readiness generation that has still not passed its `ready` probe
violates it, once per generation. The details give the generation and the quiet
ticks. A node that crash-loops after an injected kill, or a cluster that never
regains readiness after a restart, reports through this assertion.

After initial readiness, an optional `workload` command starts once and remains
independent of node recovery. An optional `check` command runs serially and
continuously. The supervisor publishes the checks-started register before it
spawns each check, and after a check exits successfully it publishes the run
number, the check's pid, and the disturbance-generation range the check
spanned. The host treats the Sometimes and Reachable assertions that pid passed
after that start as the check's evidence; a check that passed none leaves the
prior evidence intact. Process transitions and accepted instrumentation reports
advance the disturbance generation, which keeps stale pre-fault evidence
distinct from a check completed after recovery. Each check receives its starting
generation in `HARMONY_DISTURBANCE_GENERATION`, allowing a stateful checker to
invalidate cached results without importing process-fault semantics.

Instrumented nodes receive a pair of inherited event descriptors. The generic
control and report frames live in `process-proto`; the supervisor acknowledges
runtime readiness, orders arms and disarms, and only credits an event kill when
its report matches the acknowledged rarity and window-start identity. A report
that arrives before its arm acknowledgement has been fully read stays buffered
until that acknowledgement is available. While a
park is in effect, each tick's status reply gives its fire count and whether a
thread is held. The runtime acknowledges a disarm at once, and the park stays
in effect until a status reply shows it neither armed nor holding a thread.
The event-park-held register has one bit per node with a held thread, and the
event-kill-armed register has one bit per live node with an acknowledged event
kill that has not fired. A node's held bit clears at a status reply without a
held thread or when the node dies or restarts. A paused node answers no status,
so its held bit keeps its last value until the node continues. When
multiple event-kill windows overlap for one node, a reported kill advances the
supervisor to the next unfired window identity. Outstanding windows, commands,
arms, and a reported kill awaiting observed child death contribute to the
pending-fault fence. A protocol failure while work is outstanding marks the
execution as an infrastructure failure. A kill or restart retires the node's
channel before it signals the process. The killed process can close its
descriptors before the supervisor reaps it, so a retired channel ignores a
closed transport.

Each tick, a ready channel with nothing in flight also asks its runtime for
edge-coverage status. The supervisor adds each node's new bucket crossings and
digest change to the edge-crossings and edge-digest registers. These are
coverage evidence only: they do not advance the disturbance generation or the
pending-fault fence.

A process window addressed to node `EVERY_NODE` (`u16::MAX`) applies its
kill, pause or restart to every node at once.

The faults workload owns the semantic fault policy and composes its optional C
instrumentation runtime with `libvoidstar`. The supervisor consumes only the
generic process actions and event protocol and does not depend on that workload.

`bundle`, `reconcile`, `recovery`, and `supervise` are portable library
modules. Linux device and process wiring is isolated to the binary. The
standalone crate can be checked on a development host with:

```sh
cargo test --manifest-path consonance/harmony-linux/supervisor/Cargo.toml
cargo clippy --manifest-path consonance/harmony-linux/supervisor/Cargo.toml --all-targets -- -D warnings
```

Miri covers the portable parsing and reconciliation modules. Process integration
tests run natively: Miri cannot execute the credential, spawn, and wait syscalls.
The native suite checks process-group isolation, descendant cleanup, and readiness
probes alongside running nodes; the OCI fixture checks guest credentials. Linux
event-channel socket regressions run natively because the pinned Miri interpreter
does not support their nonblocking ioctl. Descriptor duplication and ownership
remain covered under Miri.

## Investigation terminal

The supervisor polls the process-proto debug namespace alongside standing policy.
When enabled by the host, it runs a shell or uploaded script on a guest PTY with
the workload environment, credentials and working directory. The image supplies
`/bin/sh`. The helper creates a session and controlling terminal, preserving job
control and terminal signals. Input is acknowledged once; output and status are
SDK events. Ordinary execution replies with no debug command. The terminal, its
children and filesystem changes live entirely in guest memory and snapshots.
The host saves the post-session snapshot rather than replaying interactive input.

The shell helper resets inherited terminal signal dispositions before exec;
otherwise the background runtime launcher can leave child commands ignoring
Ctrl-C. Linux unit and CLI PTY tests exercise this path. Miri does not model the
`signal` syscall; the syscall test is excluded there, while terminal buffer and
protocol tests remain interpreter tests.
