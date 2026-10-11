# Consonance client

`Client<T>` negotiates the control contract over a synchronous `Transport` and
preserves protocol failures as errors distinct from workload stops. The optional
`in-process` feature implements transport for `ControlServer`.

The SDK catalog decoder resolves named state registers and validates declarations
and values. The current SDK event stream carries no publisher identity; this
reader accepts one catalog per evidence stream and rejects multiple publishers
rather than merging ambiguous register coordinates. Event 0 also carries
Antithesis JSON records; the reader skips payloads that begin with `{`. Read a fresh catalog/state
view at the stopped snapshot evidence cut when constructing a workload driver.

`session::Session` is the optional in-process composition layer used by
workload adapters that boot a guest directly. `SessionConfig` makes RAM,
seed, run budget, command line, and identity domain explicit. The session
owns setup, branch, replay, run, read, and SDK-event operations, while
workloads retain only their observation and action codecs. `PortableSnapshot`
keeps the original full-copy archive format used by the fault workload.
Sessions release completed host trace segments after successful branch/replay
operations, keeping their evidence storage bounded by the active segment.
Callers that archive normalized exit traces use the control server's trace API.

`SessionConfig::with_nested_host()` selects the named nested-host contract on
Linux x86, using VMX or SVM according to host KVM support. The choice travels through worker configuration and changes image
identity even with an identity tag, so ordinary and nested snapshots cannot
share a cache domain. It requires the matching KVM-enabled guest kernel and
host nested VMX or SVM. `Session::branch_with_seed` restores a held snapshot and selects
the SDK entropy stream for its continuation; the nested search adapter records
that seed as its action.

A package that answers its own opaque service requests installs a resolver with
`Session::set_service_factory` and branches with `branch_with_service`, which
carries the package's `ServiceConfig` into the branch so the control server
builds that handler. The handler is built before the live VM changes, so an
uninstalled configuration fails the branch and leaves the session untouched.

The same call carries the host-plane effects the run after the branch applies,
each against the virtual moment it lands at, so a package stages a machine-level
perturbation without reaching past the session boundary. The control server
checks every effect against the branched snapshot before the live VM changes: a
moment behind the snapshot, a moment already occupied, an out-of-range address, or a backend that cannot arm
the exact-count arrival all fail the branch with the session untouched. One
moment carries one effect, so a duplicate is reported rather than overwritten.

`Session::run_until` runs to an absolute virtual-time deadline or an earlier
stop. `Session::snapshot` captures that exact stopped state in one control
exchange and returns the server's synchronized V-time. It never advances the
guest or retries a refusal, so a capture failure is returned to the caller with
the control diagnostic.

`SessionConfig::defer_virtual_time_checkpoint_hashes` moves sparse
virtual-time checkpoint hashing out of the run that reaches a checkpoint. Each
due checkpoint otherwise hashes all of guest RAM inside that run, which a
gigabyte-class guest cannot afford during boot. The session applies the setting
to every VM it boots, including the ones a restore boots from its factory, and
before the guest runs, so the boot is covered. The setting is off by default,
changes neither guest state nor the normalized event sequence, and stays
outside the session identity; a composition root that wants the hashes installs
them afterwards with `Vmm::checkpoint_virtual_time_trace_at`.

`SessionConfig::wall_limit` bounds the uninterrupted host time in which one run
makes no deterministic virtual-time progress. A slowly advancing instrumented
guest can take longer than the bound in total, while a guest spinning at one
virtual moment is abandoned through the backend's cancellation latch and
reported as `SessionError::Hung`. A canceled VM cannot be entered again, so
every later request on that session reports `SessionError::Abandoned`. A
backend without both a cancellation latch and the in-process virtual-time
progress clock reports `SessionError::Unboundable` on the first bounded run.
The limit is a host resource bound, so it is deliberately outside the session
identity and the image identity. The `watchdog` module owns the mechanism and
reserves SIGUSR1 process-wide, so every composition that arms a host bound
shares this one guard. A request that returns as the bound expires claims the
run and keeps its reply.

`SparseSnapshot` is the explicit `consonance-whole-vm-v2` archive shape used
by adapters that need page and sidecar sharing across related checkpoints.
Its serde fields remain `base`, `image_identity`, `pages`, and `sidecar`; the
sharing metadata is host-local and is never written to the wire. An export
base must have the same setup and identity, and unchanged pages/chunks are
retained by reference until a snapshot is serialized.

The embedded complete and sparse portable snapshots use format version 6.
Older embedded versions are rejected. The outer sparse archive layout remains
version 2.

For matched profiles, logical identity excludes only validated init x87/SSE raw
presence metadata; exported artifacts retain those bytes and checksum them.
The guest execution restrictions and import boundary are documented in the
[core identity contract](../vmm-core/README.md#published-xsave-identity-check).

`cache` holds one snapshot cache for all workers of a search. A namespace
separates sessions with different images, configuration, setup state, or
service. `SearchSession::cache_identity` names the setup state. It is the
state hash, except for User-mode Linux, whose setup image holds per-process
host values, so it also hashes the whole setup checkpoint. Within a namespace an entry's key is the byte encoding of its action
prefix, with fixed-width actions, so a cached prefix of an input is a byte
prefix of its key. `CacheIndex` is the interface workers use:

| Call | Result |
|---|---|
| `lookup(namespace, key)` | a lease on the longest cached prefix |
| `extent(len)` | a writable region, after evicting entries to fit the budget |
| `publish(namespace, key, parent, extent, cost)` | a lease on the committed entry; a duplicate key keeps the first entry and frees the new extent |
| `chain(lease)` | the entry's extents, anchor first |
| `release(lease)` | the entry can be evicted again |
| `report_store(holder, bytes)` | records a worker's local store size, evicts to fit, and answers whether that worker should shrink |
| `forget_store(holder)` | removes a finished worker's store from the budget |

`LocalIndex` is a mutex over the table, held by the process that runs the
search. Extents live in 64 MiB `PageSegments` backed by memfd on Linux and
`shm_open` on macOS. Committed extents are never written again. An extent holds
the pages a snapshot changed over its parent with their BLAKE3 hashes, the pages
that returned to the setup content, and the sparse sidecar, with a SHA-256
checksum over everything except the page data. Every 32 entries along a chain
the next entry stores its full page list over setup and becomes an anchor, so
an import reads at most 32 extents.

Eviction takes, among entries with no lease and no children, the one with the
lowest priority, using GreedyDual-Size (Cao and Irani, 1997). An entry's cost
is the work needed to rebuild it from its parent; the faults workload passes
the guest time of the action that produced it. When an entry is published or
leased its priority becomes a floor plus its cost per extent byte, and each
eviction raises the floor to the evicted priority. A cheap, large entry goes
before a costly, small one, and an entry nobody leases falls behind as the
floor rises. A search that re-runs evicted prefixes spends most of its extra
time on the costly ones, so this rule keeps them.

On Linux a freed extent's pages are released by punching a hole in the
segment's memfd, and the budget stops counting the extent at once. An extent
that a caller still holds from `chain` keeps its pages, and the budget counts
it until its segment dies. Elsewhere the budget counts every allocated extent,
including freed extents in segments that still hold live ones. A segment is
unmapped when its last extent dies. Each live segment holds one descriptor, so
a faults search raises its soft open-file limit to the hard limit on Linux, and
the index keeps its mapped segments, including those that only a caller's
`chain` copy still holds, within half of the soft limit. An extent that needs a
new segment at that limit evicts entries until a segment dies, or is refused. The budget also counts the bytes
each worker's local snapshot store reports, so the cache gets what the stores
leave. When nothing can be evicted the index refuses the extent and the worker
keeps its snapshot local. A report that leaves the total over budget after
eviction asks the worker with the largest store to shrink.

`Session::publish_snapshot` exports a delta straight into an extent and
publishes it. `Session::import_cached` resolves a chain into one page list over
setup and imports it; the store checks each page against its hash.
`cache::plan_memory` takes the free memory (the lower of `MemAvailable`
and each enclosing cgroup's limit minus usage on Linux; free, file-backed and
purgeable pages on macOS) less a 1 GiB reserve, fits as many workers as the
cores allow with room for each guest and a minimum store, and gives the rest,
after the guests, to the budget. It fails when one worker does not fit.

`WorkerSession` runs a `Session` in a child process, because Hypervisor.framework
allows one VM per process. `WorkerLauncher` starts the child with one end of a
Unix socket pair, named by `HARMONY_SESSION_WORKER_FD`, and the child serves it
with `serve_inherited`. The child re-executes the same binary, so it carries the
same HVF entitlement and inherits the calling thread's CPU affinity. Each call is
one length-prefixed request and reply. Guest hangs, abandoned sessions and other
errors come back as the matching `SessionError`; a closed socket or a dead child
marks the session abandoned. The index and every lease stay in the parent. A
publish asks the child for the delta size, allocates the extent, and sends the
segment's descriptor so the child writes the delta into the shared mapping; the
parent publishes only after the child answers. An import sends one descriptor per
extent in the chain, and the child maps each range read-only for that call.
A child that dies mid-call therefore leaves no entry half-written and no lease
held; its unpublished extent is freed like any abandoned extent. Dropping a
`WorkerSession` closes the socket and reaps the child.

`UmlSession`, behind the `uml` feature on Linux, runs a User-mode Linux guest
through `uml::Session`. `UmlLaunch` names the profile, initramfs, memory, boot
arguments, seed, setup budget and the progress limit after which a guest that
makes no virtual-time progress is reported as `SessionError::Hung`. Boot runs
the guest to its setup snapshot point and captures it. Each snapshot is a full
`uml::Checkpoint`; a restore is in place while the guest is paused and starts
a fresh process otherwise. A publish exports the checkpoint into one extent,
and an import reads it back with the session's service factory. Branches
refuse mechanical effects, because the UML backend cannot write guest memory
or inject interrupts. `store_bytes` is the allocated size of every held
checkpoint, and telemetry reports the count and host time of captures,
restores, fresh restores and imports.

`SearchSession` is the set of calls a search worker makes, implemented by
`Session`, `WorkerSession` and `UmlSession`.

`placement::CorePool` finds the host's fastest core type within the process's
CPU affinity, cut to the whole cores of the tightest cgroup `cpu.max` quota: the `cpu_core` and `cpu_atom` lists on hybrid x86, the part
number in `MIDR_EL1` on arm64, and `cpu_capacity` or the maximum frequency to
rank the types. Its cores are ordered fastest first. `CorePool::plan` gives
each worker its own core and the coordinator the slowest remaining one, and
refuses more workers than the pool leaves room for. A pool of one core holds
one worker and the coordinator together. `pin_current_thread` binds
the calling thread to one core on Linux. A worker's guest runs on the thread
that owns its session, so pinning that thread keeps the guest on one core. On
macOS the pool counts the performance cores and nothing is pinned.

```sh
cargo test -p consonance-client
cargo clippy -p consonance-client --all-features --all-targets -- -D warnings
```

`Session::read_observation` reads a bounded range of a kernel-published observation
handle while execution is stopped. Registration is resolved from SDK events in
the current snapshot, so restoring an earlier snapshot also restores its handle
lifetime. Unknown, revoked, malformed, and out-of-bounds observations fail before
a guest-memory read. Large observations are fetched in chunks within the control
protocol's read limit while the guest remains stopped. Workload adapters own
interpretation of the returned bytes.

The default x86 command line preserves AVX while disabling XSAVEOPT and XSAVES
selection, and passes `LD_BIND_NOW=1` to PID 1 before its libc startup. Custom
command lines used for XSAVE qualification must retain those settings. The
kernel, pinned runtime re-execution, and admitted workload environment have
separate checks; default boot arguments alone do not certify an arbitrary image.

Session console diagnostics return the most recent 64 KiB. The VM reader pages
from the tail offset reported by the guest, so long boot output does not hide
later failure evidence. UML uses the same bounded-tail contract. Repeated reads
are snapshots and may overlap; reading diagnostics does not advance execution.

Durable roots use the `SearchSession` root methods, separately from the transient
cache namespace. Hardware roots remain deltas against the verified deterministic
setup. UML roots contain all nonzero guest and private-image pages, plus service
sidecars, and bind the profile, initramfs, memory, arguments and seed. They can be
imported over a fresh launch whose host-private setup bytes differ. Cache deltas
still require their original setup identity. Neither interface interprets
workload commands.
