# Faults workload

`faults-workload` searches a deterministic replicated-service workload.  Each
campaign action is an eight-byte package record containing a catalog index and
bounded logical work/recovery durations.  The v2 catalog includes a persistent
five-millisecond directional delay in addition to partition, recovery, pause,
restart, and advance actions.  The guest supervisor translates that record into
the standalone `fault-runtime` action vocabulary, runs the declared check
command, and publishes the result through the Harmony SDK.

The host adapter uses the workload-neutral `consonance-client::session::Session`
with this package's explicit RAM, seed, command line, and run-budget contract
for one-vCPU whole-VM branches and sparse portable snapshots. Each action has
a five-second virtual-time limit covering fault execution, recovery, and checks;
the resolved execution identity records this resource contract.  The campaign
uses `searcher::search::rollout::execute_suffix`; action iteration and probing
remain owned by the generic search engine.

The OCI rootfs supplied to `search_consonance` must contain
`/harmony/workload.json`, the fault guest supervisor, and the declared node
programs.  Schema version 2 may also declare a `pending` command.  A
successful pending command must verify that the primary's replication request
was acknowledged by the replica but remains unapplied; the recovery command
must verify delivery before the guest clears its pending-work register.  The
JSON schema is [`WorkloadSpec`](src/spec.rs).  The package
appends its control cpio archive after the compressed base and OCI members and
pads that boundary to four bytes, which is required for Linux initramfs to
recognize the raw `newc` header. The package init exports
`PATH=/sbin:/usr/sbin:/bin:/usr/bin` to the supervisor and its children so
network tools installed by the OCI image resolve consistently.

`fixtures/replicated-service` supplies a runnable two-process image contract:
the primary acknowledges writes before replication, the check reads the
replica, and the recovery command synchronizes it after a directional
partition. Its `setup` command creates separate network namespaces and the
veth endpoints named by the fixture's JSON; the replica assertion uses a Unix
control socket, so Linux `tc` enforcement exercises only the replication path.
Its pending command provides an application-level queued-but-unapplied
acknowledgement, so the delay snapshot records real outstanding work rather
than inferring it from an installed qdisc or host timing.
The same one-vCPU guest runs this setup and the pure [`oracle`](src/oracle.rs)
tests keep the expected stale-read and convergence path stable alongside that
executable fixture.

The Linux/KVM acceptance binary `fault-vm-oracle` stages that fixture from an
OCI Docker save archive and checks the live contract in order: a readiness
probe must complete before SDK setup, a nominal action must pass, a real
directional `tc` partition must expose the stale read through the independent
Unix assertion channel, restoring the origin and replaying an action must
produce the same whole-VM state hash, an active delay must restore and recover
with queued work delivered to the same continuation, a replica restart must
pass bounded readiness and restore to the same continuation, pause/resume must
restore identically and complete checks after the replica resumes, recovery must
converge, and a bounded
Dissonance campaign must retain a violating suffix. The CI job builds an
Alpine rootfs with `iproute2` and `procps`, plus static guest and fixture
executables, so the network fault is exercised inside the guest namespace.
The shipped acceptance artifacts target `x86_64`; an aarch64 caller must
supply a full Linux guest image with procfs, sysfs, devtmpfs, script loading,
futexes, `/dev/mem`, network namespaces, veth, and netem enabled. The
repository's freestanding arm64 M1 image remains a separate minimal profile.
