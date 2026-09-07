# Replicated fault fixture

This image-side fixture is a runnable known failure for `faults-workload`.
`fault-replica` runs a primary and a replica in separate Linux network
namespaces connected through a routed pair of veth endpoints.
The primary acknowledges a write without requiring replication to succeed. The
`fault-check` command writes to the primary and reads the replica, so a
primary-to-replica partition produces a stale read. The optional
`fault-pending` command sends a `REPL_PENDING` application message and requires
the replica to acknowledge it as queued while keeping GET unchanged; this is
issued-but-unapplied state captured under `DelayForward`.
`fault-recovery` sends the primary's current value after the fault is removed
and verifies that the replica's pending entry is gone and its visible value
matches before succeeding. It retries the complete sync/read/status
verification up to 16 times, sleeping 10 ms of guest time between attempts;
the final protocol error is returned if the bounded window is exhausted.

Build the five binaries for the guest architecture, place them at the paths
in `workload.json`, and copy the workload assets to `/harmony`. The workload's
`setup` command runs `network-init.sh` once before nodes start. The supervisor's
`PartitionForward` action applies `tc qdisc loss 100%` to root-side `veth-0-1`;
its `RecoverForward` action removes that qdisc and invokes the recovery
command. `DelayForward` applies a deterministic five-millisecond directional
delay; the pending command observes application-level queued work rather than
claiming that a host TCP packet remains in flight. The replica GET/STATUS
assertions use its Unix control socket, so network faults exercise only the
replication path.

On a Linux host with `ip`, `tc`, and `CAP_NET_ADMIN`, `smoke.sh` launches the
same binaries directly and checks the nominal, queued-pending, stale-read, and
recovery paths with real directional qdiscs and separate network namespaces.
The guest search uses the same protocol through the supervisor and then
exercises whole-VM snapshots through Dissonance.

`fault-ready` probes both control endpoints before the guest publishes SDK
setup completion. `fault-check` exits 1 only for the intended stale-read
invariant and exits 2 for transport or startup errors. `fault-pending` and
`fault-recovery` exit 2 for any failed protocol or delivery assertion, which
keeps command failures out of the pending-work observation and Dissonance
victory stream.

Replication attempts use a 100 ms timeout; control requests allow two seconds
for that attempt and its response. Recovery's retry window uses guest sleeps,
so it gives the replica time to become reachable after qdisc removal without
claiming success before the value and pending state are verified. A failed
client connection ends its own request while the replica continues serving
recovery commands. The loopback process regression exercises both a stalled
replication peer and an incomplete control request; the Linux smoke gate
requires the partition check to exit with the stale-read assertion code
specifically.

The fixture is intentionally small enough to use in a one-vCPU OCI image. It
also gives a package acceptance run a real process, real TCP messages, a real
directional Linux network fault, and a check/recovery pair whose exit status
is surfaced through the SDK.
