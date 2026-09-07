<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# fault-runtime

`fault-runtime` is the guest-side supervisor for deterministic workload fault
injection.  It owns a stable node inventory, applies a versioned action stream,
and keeps running when one managed process exits.  The crate has no dependency
on Consonance, searcher, or a host execution API; callers provide an explicit
[`Backend`](src/lib.rs) implementation.

The action stream uses schema version `1` (`ACTION_SCHEMA_VERSION`).  Actions
are applied in the order received.  A backend operation runs before its state
change is committed, so a failed `tc`, process, failpoint, or custom command
cannot be reported as a successful fault.  `Advance` is the only operation
that moves supervisor time.  Recovery windows open at the current tick and
expire when the tick reaches their exclusive end; a window can target one node
or all nodes.

Each node has a stable numeric `NodeId`, an executable, arguments, environment,
and a persistent directory.  `Kill` removes only the managed process.  `Restart`
starts the same node specification and leaves its directory intact, allowing a
workload to recover state from files after a failure.  `Pause` and `Resume`
require a live process and preserve the same process handle.

Network faults are directional.  The topology must declare a separate
`NetworkPath` and egress interface for every direction that may be faulted;
there is no inferred or broadcast path.  Partition drops all packets.  Loss
faults compose by retaining the product of their success probabilities, while
delay and jitter add with saturating arithmetic.  Recovering one fault reapplies
the remaining effective rule, and recovering the last one removes the qdisc.

On Linux, `linux::LinuxBackend` starts commands directly, uses `kill -STOP`,
`kill -CONT`, and `kill -KILL` for process controls, and invokes `tc qdisc`
with argument arrays on the declared interface.  Failpoints call the node's
configured control executable as `--failpoint <name>`; custom actions execute
their declared program and arguments in the node directory.  No operation is
run through a shell.  Guests must provision the interfaces and control
executables described by the topology before starting the supervisor.

The pure supervisor can be tested with a fake backend, which is how ordering,
directional isolation, overlapping recovery, persistent restart directories,
recovery-window expiry, and backend error propagation are verified without
network access or root privileges.
