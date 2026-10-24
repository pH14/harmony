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
violates it, once per generation. A node that a kill or power-loss window left
down counts as a fault still in effect: both keep their node down until a later
restart window, so the 30 seconds start only once every node is meant to run. The details give the generation and the quiet
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
kill, pause, restart or power loss to every node at once.

The faults workload owns the semantic fault policy and composes its optional C
instrumentation runtime with `libvoidstar`. The supervisor consumes only the
generic process actions and event protocol and does not depend on that workload.

`bundle`, `disk`, `reconcile`, `recovery`, and `supervise` are portable library
modules. Linux block-device wiring lives in the Linux-only `blockdev` module,
and process wiring is isolated to the binary. The
standalone crate can be checked on a development host with:

```sh
cargo test --manifest-path consonance/harmony-linux/supervisor/Cargo.toml
cargo clippy --manifest-path consonance/harmony-linux/supervisor/Cargo.toml --all-targets -- -D warnings
```

Miri covers the portable parsing, reconciliation, and write-log replay modules.
The device-mapper and block-device ioctls in `blockdev` need a guest kernel and
run only in guest executions. Process integration
tests run natively: Miri cannot execute the credential, spawn, and wait syscalls.
The native suite checks process-group isolation, descendant cleanup, and readiness
probes alongside running nodes; the OCI fixture checks guest credentials. Linux
event-channel socket regressions run natively because the pinned Miri interpreter
does not support their nonblocking ioctl. Descriptor duplication and ownership
remain covered under Miri.

## Node disks

A bundle line `disk <node> <path> <size> [checksummed]` gives the named node
its own ext4 filesystem mounted at `path`. The supervisor accepts the optional
`checksummed` flag and ignores it; the faults search reads it to decide which
disks draw corruption faults. Sizes take a `K`, `M` or `G` suffix and must be
whole 4 KiB pages from `16M` through `1G`; a bundle has at most five disks. The
supervisor builds each disk before the setup command runs:

1. Three guest RAM disks (`brd`, 1 GiB each, pages allocated on write) hold the
   data, the write log, and the replay base. The guest kernels set
   `CONFIG_BLK_DEV_RAM_COUNT=16` and `CONFIG_BLK_DEV_RAM_SIZE=1048576`.
2. A `dm-log-writes` device named `harmony-log-<node>-<generation>` writes
   through to the data disk and logs every completed write, flush, FUA write and
   discard, with the flags of each.
3. A linear device named `harmony-disk-<node>-<generation>` maps the whole log
   device. The filesystem lives on this device, and disk fault windows swap its
   table.
4. The image's `mkfs.ext4` formats it with the execution uid and gid as root
   owner. The supervisor mounts it with shared propagation, removes
   `lost+found`, syncs, and adds a `harmony-formatted` mark to the log.

A power-loss window (`PowerLoss { choice }` in `process-proto`) kills its node
as a kill window does and keeps it down until a later restart window. It then
cuts the disk:

1. It suspends both devices without flushing or freezing the filesystem and
   swaps in `error` tables. Unwritten page cache never reaches the data disk,
   and the old `dm-log-writes` target writes its end mark.
2. It detaches the mount and requests deferred device removals, so other
   nodes' mount namespaces can let go of the old filesystem later.
3. It reads the log ([`disk`](src/disk.rs)). Every write before the last flush
   and every FUA write is kept. Of the other writes after that flush, a prefix
   in log order is kept: its length is the choice, mixed with the node index,
   modulo the number of such writes plus one. A choice of zero keeps none.
4. It applies the kept writes to the replay base, which then holds the disk as
   it would be after the power cut, and copies the base back over every range a
   dropped write touched. The data disk then equals the base.
5. It zeroes and discards the log, attaches new devices under the next
   generation's names, and remounts. ext4 replays its journal on mount.

A log that fills its RAM disk stops logging and has no end mark; the supervisor
reports that as an infrastructure error. A power-loss window for a node without
a disk acts as a kill. Every disk and log page lives in guest memory, so each
disk can cost up to twice its size plus the writes since the last power loss;
raise the guest RAM (`ram_mib`) to match. The OCI runtime allows the container
every block device and `/dev/mapper/control`; the supervisor creates the device
nodes itself.

### Disk faults

Three more windows act on a node's disk:

- A disk error window (`DiskError`) loads an `error` table. Every read, write
  and flush fails with `EIO` until the window closes and the linear table
  returns. When a journal write fails, ext4 aborts the journal and refuses
  writes until a later power loss or corruption remounts the disk.
- A disk stall window (`DiskStall { hold_nanos }`) loads a `dm-delay` table
  that holds every request for the window's length, at least 50 ms. dm-delay
  serves shorter delays from a polling kernel thread instead of a timer. When the window closes, the
  linear table returns and dm-delay submits the held requests at once. Before
  the supervisor starts a node again, it releases the held requests so the
  old process can exit.
- A disk corruption window (`DiskCorrupt { choice }`) acts as a restart window:
  the node is killed when it opens and started when it closes. In between, the
  supervisor syncs the filesystem, cuts the disk as a power loss does but keeps
  every logged write, and flips one bit in the data disk and the replay base.
  The bit lies in a plain write logged after the format mark or the last cut: a
  write with no flush, FUA, discard or metadata flag. ext4 tags its journal
  writes as metadata, so the bit lands in file data or in metadata written back
  in place. The choice, mixed with the node index, picks the write, the sector
  within it, the byte and the bit. If the disk then fails to mount, the
  supervisor formats a new filesystem on it and logs the mount error.

When an error and a stall window overlap, the disk returns errors. A power loss
or corruption remounts the disk with the linear table and then loads the table
of any open disk window. After any of these windows, the supervisor does not
report the node exiting on its own for the rest of the execution, because a
node may stop on purpose when its disk fails. A node that exits during an
error or stall window stays down until the window closes, then restarts after
a fault. For a node without a disk, error and stall windows do nothing and a
corruption window only restarts the node.

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
