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

## Transactional execution endpoints

The endpoint transaction owns one stopped cut and at most one provisional
snapshot per session. `Execute` carries an optional parent plus bounded opaque
`InputSpec` reproducer bytes, ordinary `StopConditions`, an optional exact
`Resolution`, an SDK cursor, a finite snapshot stop mask, and an explicit
whole-state/history evidence requirement. Branch and resolution are mutually
exclusive. The request digest binds all these fields. A known parent's retained
SDK count validates its cursor, rather than the unrelated current live cut; a
backend without that metadata validates against the actual selected parent
before guest execution. Unknown masks, malformed input and unsupported evidence
are refused before changing the machine.

A `Decision` returns the original stop, moment, question and identity under an
owned token. `ExecutionDecision` borrows the transport while the composition
consults its service handler. `Resume` supplies an explicit resolution and binds
its digest to the previous request. `FinalizeDecision` instead captures that
exact stopped question without any guest resume; the caller explicitly chooses
whether it needs a snapshot. Its receipt still records `Decision`, even when a
workload interprets that boundary as its own deadline. Dropping an unresolved
Decision owner abandons the session. Other stops retain their actual stop kind. The caller's snapshot mask owns seal
eligibility: a deadline-only workload supplies `EndpointStops::DEADLINE`; the
execution mechanism assigns no workload meaning to a deadline or terminal stop.
Required whole-state plus ordered-history evidence is either available or refused
before mutation. `CapturedWholeStateAndHistory` requests those facts only when a
snapshot is captured, leaving terminal uncaptured cuts explicitly `Unavailable`.
Explicitly omitted evidence is also `Unavailable`, including UML; an operational
hash failure never becomes an absent or fabricated commitment. UML accepts only
a finite deadline with the assertion stop bit armed, matching its existing run
semantics; unsupported masks are refused before mutation.

Successful execution performs the existing branch, run and optional snapshot
operations, then yields bounded SDK pages under a nonreused token. Every page
repeats its immutable request digest, stop, source count and start/end cursor
binding. Each request must use the exact next cursor and cannot read beyond the
captured end. Empty premature pages, count overflow, oversized pages and changed
bindings fail closed. The legacy final empty SDK read remains. `Commit` requires
all pages, calls the existing whole-hash and ordered-history methods, and only
then transfers the optional snapshot handle in its receipt. A snapshot handle,
whole-state hash and ordered execution history remain separate facts. This first
implementation does not fuse CPU captures or remove a SHA pass.

`ExecutionEndpoint` safely borrows its transport until commit or abort. Drop
aborts an uncommitted provisional snapshot; no self-reference or unsafe borrow
is involved. Low-level mutation methods also reject a pending endpoint, including
the now-fallible `set_service_factory` and `set_max_chain_len` policy setters, and
UML shutdown/root import. An initial Page cannot replace Decision handoff; only
explicit Decision finalization can produce a Page for that actual stop. Tokens
are session-scoped and never reset by replay, branch, import or abort. SDK cursors
remain transport offsets: a workload must still validate its decoder's lineage
and checkpoint binding. Normal abort releases the provisional snapshot and leaves
a successfully stopped cut; it does not roll guest execution back.

Failure after selecting a parent or starting execution abandons the executable
continuation, preserving the original typed error. The direct Session marks
itself abandoned before `ControlServer::abandon_owned_session` retires the VM and
releases that server's exclusively owned snapshot references and sidecars. This
includes a snapshot minted before a failing final control guard hid its ID from
the client. Cleanup runs no control or guest command, preserves failed-VM console
and SDK observations, and keeps the store's cumulative host counters. It neither
revokes independently published cache extents nor releases another session's
references. A commit reply loss or final transport guard failure abandons the
whole worker/session, so ambiguous transferred ownership cannot remain in a
reusable session.

The worker extension preserves every existing inner control-before/after and
SDK response guard; it initially amortizes only outer exchanges.
Execute plus Commit is the common two-exchange path, with bounded page exchanges
when needed. New frame bounds must be checked after an opcode prefix and before
allocating an endpoint request body. Page, commit, abort and Decision
finalization have their own small bounds; Execute and Resume have bounds derived
from existing input/control payload limits. Both hosts reject descriptors in a
header remainder or body. INIT/import keep their existing independent contracts. Endpoint replies have a fixed receive bound, forbid descriptors and
validate every tag, length and trailing byte before acting. Versioned peers must
agree on worker image protocol v5 rather than using a compatibility fallback.
Endpoint error details are bounded to 64 KiB at UTF-8 boundaries while Hung,
Infrastructure and Abandoned retain their typed wire identity. Success and error
replies also report retirement, so even a successful Decision abort cannot leave
the parent believing its worker is reusable.

Implementation milestones are one reviewable sequence: pure transaction engine,
safe direct owner and failure-retirement witness; bounded worker protocol and
explicit UML adapter with transport/guard/failure tests; then workload adoption
that preserves external decisions, caller-owned capture eligibility and decoder
provenance. Legacy-versus-batch comparisons cover exact stop/state/history/event
facts, partial failures and physical transport counts on one fixed build. Pure
engine/RAII tests are interpreter-reachable under Miri. Native snapshot/guard
witnesses and the actual repository pre-push hook qualify the final source.

The image identity always includes the loaded Harmony executable, kernel,
initramfs, RAM size, seed, setup run budget, command line, and guest contract.
Sessions booted with setup payloads also bind their ordered, length-delimited
payload bytes; the empty list keeps the ordinary session identity.
`identity_with_config_and_payloads` computes the same identity before boot and
returns an error if the loaded executable cannot be identified.

Public identity admission requires a strict host in every build, including
callers of the library API. On Linux, that host is a fully static ELF executed
from a write-sealed memfd. Admission rejects a program interpreter, dependent
libraries, writable executable segments, unsupported executable mappings, and
ambient loader overrides. It binds the sealed executable to the kernel-loaded
mapping and measures the complete executable-map census, including the vDSO,
each time the guard is checked. Admission compares every live executable byte
with the sealed file and measures the full file. Each recheck requires every
live code and vDSO page to be unwritten, so a code write since admission fails
the check even after its bytes are restored. `host-code-identity` describes
this check. Session, cache, artifact and control boundaries run the full
recheck, which also requires the same sealed file and executable mappings. The
VMM's SDK response site runs only the page check on Linux and the full recheck
on macOS, once per SDK exchange. Every recorded
outcome is produced inside a control request, which full rechecks bracket, and
a code write leaves a copied page that the next full recheck refuses. An
ordinary GNU executable and a dynamic library whose constructor clears its
loader variable are refused.

On macOS, admission requires the kernel-loaded main executable's CDHash to
match a strictly validated signed image with hardened runtime, the Hypervisor
entitlement, and the exact OS-only library constraint. Its loaded-image census
must stay within the permitted OS cache. In-process sessions, UML sessions,
session workers, cache operations, and service exchanges recheck strict host
identity at their operation boundaries. The macOS recheck uses kernel-loaded
signing state and CDHash, dyld generation, and the shared-cache census;
path-based worker launch repeats full static signature validation. A changed
or unavailable identity fails the session before a result is accepted.

Cache publication checks strict host identity after the index callback. If it
changed, the index revokes the committed entry and any descendants before the
session returns an error. Another thread can observe a published entry before
that check completes, so this guard is not an atomic cross-thread publication
fence; each consuming session checks its own host identity before using the cache.
`Session::image_identity()` returns the identifier admitted at construction;
artifact and guest-operation boundaries perform the live identity checks.

A nonempty `identity_tag` adds a workload domain; it does not replace those
setup inputs.

On Linux, `cargo run` and an ordinary GNU executable cannot create sessions or
workers. The product CLI routes Consonance work into its sealed static host.
Native CLI manifests use
`native_loaded_executable_diagnostic_identity_self` to measure their dynamic
CLI process.

Sparse snapshot pages are relative to setup RAM, so a session checks this
identity before importing pages from another session. Cache extents carry the
same identity. Snapshots made with the earlier image identity calculation do
not match the new domain.

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
An unrecorded custom answer now surfaces as `StopReason::Decision`. The package
passes an encoded answer to `Session::run` with a `Resolution`; the control
server records it before guest delivery. `SearchSession::run_resolving` carries
the same operation through worker and User-mode Linux sessions for fault search.
`branch_with_service` also accepts an optional recorded coverage schedule, so
an action's quantum is selected from input bytes at each SDK coverage exchange
without writing one answer per callback.

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

`SessionConfig::with_boot_cache(dir)` keeps the setup snapshot of a booted
session in `dir` and lets a later session with the same key restore it instead
of booting. The key is SHA-256 over the session image identity (assurance,
loaded executable, kernel, initramfs, RAM, seed, run budget, command line,
identity tag, guest contract and setup payloads), the checkpoint-hash deferral
flag, and the host: the `vendor_id`, `cpu family`, `model`, `stepping`,
`microcode` and `flags` of every CPU in the thread's affinity mask, and the
kernel `osrelease` and `version`. An entry holds the setup V-time, the sparse
setup snapshot relative to the genesis snapshot, and a SHA-256 over its header
and sidecar; the sidecar commits to the pages. A hit takes the genesis snapshot,
imports the entry with the ordinary sparse-import checks, including the genesis
RAM digest, and replays it, so the session's genesis and setup handles are the
same as after a fresh boot. A missing, damaged or refused entry falls back to a
fresh boot, which then rewrites the entry. Writes are atomic renames, and the
directory keeps the four newest entries. The cache is used only on Linux x86
and travels neither in the worker protocol nor in any identity.

`SparseSnapshot` is the explicit `consonance-whole-vm-v2` archive shape used
by adapters that need page and sidecar sharing across related checkpoints.
Its serde fields remain `base`, `image_identity`, `pages`, and `sidecar`; the
sharing metadata is host-local and is never written to the wire. An export
base must have the same setup and identity, and unchanged pages/chunks are
retained by reference until a snapshot is serialized. The embedded sidecar
binds the source setup RAM, so changing only the outer `image_identity` cannot
import its pages over a setup with different RAM.
`checkpoint_owned_bytes` bounds the copied page-index capacity and
`visit_checkpoint_allocations` enumerates immutable page, sidecar-index and
sidecar-chunk allocations with domain-separated host-local identities. An async
consumer charges them before cloning, counts each shared allocation once and
retains the cloned references while encoding. The visitor does not serialize
pointer identities or grant permission to bypass fresh import verification.
`SparseSnapshot::logical_identity` returns a copy whose x86 sidecar carries the
VMM's logical XSAVE identity projection. Hash that copy when comparing
snapshots for identity, and restore from the original.

`Session::event_history` and `SearchSession::event_history` return the cumulative
v3 ordered-history record count and commitment through both in-process control and
worker transport. Snapshot replay, portable imports and cache imports restore
the captured history; diagnostic trace retirement does not reset it. A backend
without normalized history returns an explicit unavailable error. UML does not
supply this certificate.

Completed service exchanges contribute records in addition to normalized
transport events; this count is not a guest-exit or diagnostic-log count.

Embedded complete portable snapshots use format version 11; sparse sidecars use
version 15. Older embedded versions are rejected. The outer sparse archive layout
remains version 2.

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
prefix of its key. Publishing a delta under a parent requires the parent's key
to be a proper byte prefix of the child key; unrelated ancestry is refused
before the entry becomes visible. A lease or writable extent belongs to the
index that issued it; foreign references cannot alias an entry or segment with
the same local number. `CacheIndex` is the interface workers use:

| Call | Result |
|---|---|
| `lookup(namespace, key)` | a lease on the longest cached prefix |
| `extent(len)` | a writable region, after evicting entries to fit the budget |
| `publish(namespace, key, parent, extent, target_commitment, cost)` | a lease on the committed entry; an equal-target duplicate keeps the first entry and frees the new extent, while a divergent duplicate fails without returning a lease |
| `chain(lease)` | the entry's extents, anchor first; fails if quarantined |
| `verify_namespace(namespace)` / `verify_lease(lease)` | refuses a quarantined domain or held lease |
| `verify_integrity()` | refuses any known contradiction in this shared index |
| `divergences()` | both retained witness chains, namespace, action prefix and commitments |
| `release(lease)` | the entry can be evicted again |
| `revoke(lease)` | removes the entry and descendants, invalidating any held leases after a failed post-publication guard |
| `report_store(holder, bytes)` | records a worker's local store size, evicts to fit, and answers whether that worker should shrink |
| `forget_store(holder)` | removes a finished worker's store from the budget |

`LocalIndex` is a mutex over the table, held by the process that runs the
search. Its diagnostic-only `lock_counters()` shares atomic counters across
clones; other `CacheIndex` implementations default to no counters. Active sites
report acquisition `attempts`, `acquired`, release `completed`, `wait_ns` and
`hold_ns`. Sites cover every index operation, including `stats` and abandoned
writable extents. Counter export takes no table lock and inactive sites are omitted.
Acquisition attempts are counted before the wait timestamp; wait ends just after
mutex acquisition. Hold starts at that acquisition timestamp and ends immediately
before unlocking, including bookkeeping at acquisition, allocation, eviction and
reclamation performed under the existing mutex. Completion counters update after
unlocking, including early return and unwind. Unlock and final atomic updates are
excluded. These durations include timer/atomic overhead and overlap nested
allocation/I/O work; they must not be added to those durations. Concurrent reads
are approximate field snapshots, not a synchronized event record. Abandon callbacks
hold only weak table ownership plus the shared counters. Host time never
enters `CacheStats` equality, budgeting, leases, quarantine or search policy.
The campaign reports each shared index once.

Extents live in 64 MiB `PageSegments` backed by memfd on Linux and
`shm_open` on macOS. Committed extents are never written again. An extent holds
the pages a snapshot changed over its parent with their BLAKE3 hashes, the pages
that returned to the setup content, and the sparse sidecar, with a SHA-256
checksum over everything except the page data. Every 32 entries along a chain
the next entry stores its full page list over setup and becomes an anchor, so
an import reads at most 32 extents.

Extents cross a process boundary. The reader checks the extent checksum, and
the VMM checks every imported page against its BLAKE3 hash before the snapshot
store accepts it. The store hashes a page once, when it is written or
imported, and later exports and restores read the stored hash without
hashing the page again.

`PageSegments::free` consumes a writable extent; callers cannot reclaim raw
coordinates while retaining a slice of that range. Abandonment transfers a
private, non-cloneable reclamation capability when the writer relinquishes ownership.
Committed copies share one allocation identity and retirement marker. Every
retirement verifies the actual backing segment identity, and an allocation's
accounting retires at most once even if several committed copies are freed.
Only unique ownership permits hole punching; surviving committed readers keep
their bytes unchanged. A foreign extent cannot reclaim a numerically matching
range in another allocator.

The target commitment identifies the resolved state, not the chosen delta
layout. For VMM snapshots, it binds the cache image prefix to the sparse
sidecar with only the established logical CPU-state projections: init-valued
x86 XSAVE restore presence and the Arm virtual timer's captured counter.
Every other sidecar field stays in the commitment, and the original sidecar
bytes remain in the extent for restore. Sparse delta export derives the
sidecar's target-pages digest from the hashes of all target pages that differ
from setup, independently of the selected parent;
the sidecar also carries the setup RAM digest, canonical state suffix, VM
state, SDK state, policy, control state and the separate ordered event-history
commitment/count. Equal logical endpoints reached through different normalized
histories therefore conflict under the same cache key. Publication rejects a sidecar
without that target-pages digest. For UML checkpoints, the commitment binds
the cache identity to the checkpoint digest, which covers logical memory and
bridge/service state. The cache compares commitments while holding its index
lock, before returning a duplicate lease.
A divergent publication permanently quarantines the entire namespace while
holding the index lock. Lookups produce no lease, held leases cannot import,
and subsequent publications, including extents allocated before the contradiction
and dependent descendants, fail. The namespace's entries and both complete
extent chains remain pinned against eviction and revocation. Both typed errors
(`DivergentDuplicate` and `Quarantined`) own the witness record by reference, so
its bytes survive index teardown. `divergences()` exposes the same record for
persistence. Ordinary equal-target duplicates still release the new extent.
Import and publication callers recheck held leases at completion; a concurrent
quarantine cannot return a successful imported or published handle after that
check. A consumer must also check integrity before accepting dependent work;
returned extent bytes alone cannot revoke already running guest execution.
The fault campaign performs these checks at execution, admission and final
report boundaries and refuses a completed verdict after any contradiction in
the shared index. Quarantine is intentionally permanent for this index's lifetime;
recovery requires a new investigated execution domain.

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
setup and imports it; the store checks each page against its hash and the
sparse sidecar's complete target-page commitment. Hardware
session cache extents carry the publishing session's image identity beside the
sparse sidecar. Import checks every extent's identity before changing the
snapshot store, including when a caller passes a lease from another namespace.
User-mode Linux root extents bind the launch identity, and later cache extents
also bind the setup checkpoint digest. Their imports check the binding before
creating a checkpoint, even when a caller passes a foreign lease.
`cache::plan_memory` takes the free memory (the lower of `MemAvailable`
and each enclosing cgroup's limit minus usage on Linux; free, file-backed and
purgeable pages on macOS) less a 1 GiB reserve, fits as many workers as the
cores allow with room for each guest and a minimum store, and gives the rest,
after the guests, to the budget. It fails when one worker does not fit.

`WorkerSession` runs a `Session` in a child process, because Hypervisor.framework
allows one VM per process. `WorkerLauncher` starts the child with one end of a
Unix socket pair, named by `HARMONY_SESSION_WORKER_FD`, and the child serves it
with `serve_inherited`. The child re-executes the same binary, so it carries the
same HVF entitlement and inherits the calling thread's CPU affinity.
`WorkerLauncher::current_exe` records the parent's loaded image identity and
checks the spawned child's main executable. The child then measures its own
loaded image and sends that identity before the parent sends initialization;
the parent refuses a different identity before guest setup.
The launcher checks loader overrides again immediately before spawn, and the
worker rejects them on entry before accepting initialization.
Linux checks procfs's PID namespace before reading `/proc/<child-pid>/exe` and
compares the parent's and child's admission-time executable mappings. macOS
reads each process's main code-directory hash. A replacement of the executable
pathname cannot cause a different worker image to run under the parent's
recorded identity. Each call is
one length-prefixed request and reply. Guest hangs, abandoned sessions and other
errors come back as the matching `SessionError`; a closed socket or a dead child
marks the session abandoned. The index and every lease stay in the parent. A
publish starts one EXPORT conversation. The child computes the sparse delta once
and sends NEED_EXTENT from its destination-allocation callback. The parent
allocates a unique writable extent and returns EXTENT_READY with exactly one
descriptor and the exact checked range. The child writes changed pages directly
into that mapping, drops its writer and borrowed slices, and then acknowledges
completion. The parent reads the extent and computes its commitment only after
that acknowledgement. No complete serialized payload buffer or subsequent
full-extent copy is retained in the child; page payloads still copy once into
the destination, and metadata and sidecar allocations remain. The loaded-image
handshake requires matching workers.
The version 5 host image hello identifies qualification-aware INIT, descriptor-table
IMPORT and transactional execution-endpoint commands. Both incompatible version 4
schemas are refused alongside versions 1 through 3 before initialization.
IMPORT carries a message-local
backing table: U descriptors followed by N ordered (slot, offset, length) rows.
Versions 1 through 4 are refused because those peers do not share the complete
current protocol. The protocol reference cap is independent of diagnostic
sampling limits.
The parent groups only exact live Segment Arc owners while holding the original
committed chain clones. File descriptor numbers, allocator-local segment IDs,
identical bytes and inode metadata do not establish owner identity. Both the
original N<=64 reference limit and U<=N descriptor limit remain enforced. Slots
appear in first-use order and every supplied slot must be referenced; malformed
counts, slots, overflow, zero lengths and trailing bytes are rejected before
mapping, while each mapping still validates its exact backing range.

The child still makes N read-only mappings and drops them before replying. No
registration, mapping or additional pin survives the import. Namespace, pinned
object commitment, actual pages/sidecar/state and post-reply lease checks remain
mandatory on every call. Grouping uses bounded O(N*U) comparisons. Numeric wire
metadata grows by `8 + 8*N` bytes per import versus the former envelope, even if U=N;
U=N saves no descriptors. Store intern copies,
guest materialization and SDK guard work are unchanged. Existing physical I/O
counters report actual U handles and copy counters retain actual N mappings;
backing-reuse counters retain their original N/U per accepted operation meaning.
Repeated-owner transport fixtures show fewer handles, not campaign speedup or
once-per-worker registration/shared Store pages. Dropping a `WorkerSession`
closes the socket and transfers the child to the teardown owner described below.

Worker ancillary parsing bounds every header and complete descriptor integer by
both the supplied control-buffer extent and recvmsg's actual `msg_controllen`.
A truncated Darwin control header can retain its original larger `cmsg_len`;
allocation padding is never interpreted as another descriptor. Complete returned
SCM_RIGHTS handles become owned immediately and close on framing/truncation error;
unrelated descriptors remain untouched. Pure native-layout parser tests run under
Miri; the raw truncation regression runs in an isolated subprocess and checks
received-right closure and descriptor-zero preservation. The native test checks
ownership/refusal on every supported kernel; reproducing the original declared
length mismatch depends on the kernel (observed on macOS 26.6.2). Synthetic
parser tests force the mismatched bounds independently of kernel behavior. This does not claim to
recover kernel-installed handles that the OS did not return: observed Darwin
truncation can leave unreported handles, tracked separately from bounded parsing.
The accepted worker descriptor cap remains unchanged; the version 5 host hello
identifies both the descriptor-table import schema and the execution-endpoint
protocol.

On Darwin, every worker frame read uses `recvmsg` with a separate 2048-byte
control buffer, including header remainders and body bytes. All complete
returned rights become owned before oversized messages are refused; rights
after the initial header receive are malformed and close on refusal. A plain
Darwin read can install rights without reporting them, so it cannot consume
frame remainders safely. Linux retains its protocol-sized initial control
buffer and plain remainder reads.

The Darwin bound is a qualified XNU implementation assumption, not a universal
ancillary-data API guarantee. Audited public releases
[`xnu-10002.1.13`](https://github.com/apple-oss-distributions/xnu/tree/xnu-10002.1.13),
[`xnu-11417.140.69`](https://github.com/apple-oss-distributions/xnu/tree/xnu-11417.140.69)
and [`xnu-12377.121.6`](https://github.com/apple-oss-distributions/xnu/tree/xnu-12377.121.6)
limit the complete send control allocation to one 2048-byte cluster, with a
tighter LP64 expansion limit, accept one SCM_RIGHTS header covering that
buffer, and preserve control record boundaries through receive. The bound
combines [whole-control allocation](https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.121.6/bsd/kern/uipc_syscalls.c),
[AF_UNIX header admission](https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.121.6/bsd/kern/uipc_usrreq.c)
and [control receive boundaries](https://github.com/apple-oss-distributions/xnu/blob/xnu-12377.121.6/bsd/kern/uipc_socket.c).
The per-mbuf 512-FD constant or socket high-water options alone do not prove it.

Public XNU tags are not the shipped kernel binaries. Local native evidence is
macOS 26.6.2, build 25G83, Darwin 25.6.0/XNU 12377.161.14~5, newer than every
audited tag; no matching public source was inspected. The ordinary
`Checks / Harmony Host Compatibility / macOS Arm64` job on `macos-14` checks
exact PASS records in its existing workspace test log for both native Darwin
qualification tests. Those records supply macOS 14 runtime evidence. Missing
feature selection or a skipped test fails that check without another Cargo
build. No macOS 15 native run was performed.

Sacrificial subprocess tests cover protocol limits, maximum accepted and
oversized kernel sends, multiple headers, queued sends, zero-data controls,
and unexpected rights in header remainders/body bytes with and without a
deadline. They compare live task-FD sets after closing only known owned handles.
Unexpected MSG_CTRUNC still refuses the message; XNU can install descriptors
omitted from truncated output, which this process cannot recover safely. Such
behavior falls outside the qualified bound and is not repaired by closing
guessed FD numbers. New macOS/XNU behavior requires requalification.

The parent retains checks before EXPORT, after NEED_EXTENT, after the extent
allocator callback before sending its descriptor, and after receiving completion.
Publication retains its check after commitment calculation before the index
callback, and its revoking check after that callback. Each allocation exchange
permits only one destination request. Its frames are bounded to 4096 bytes and
reject trailing bytes, unexpected descriptors, repeated requests, and ordinary
commands. A same-session nested call is rejected before any socket operation.
Child serving checks once before its first receive, after each handler/status
result before sending, and after every successful send. Within the allocator
callback it also checks before NEED_EXTENT, after sending that request, and
after receiving the allocation response. Each retained boundary performs fresh
live-code validation. Test counters distinguish guard entries from actual strict
dispatches; fixture counts establish placement, while admitted-host tests
establish strict mutation refusal. A publication retains six parent checks;
INIT plus one successful EXPORT uses eight child checks in the transport fixture.
An export uses four control frames. Copy elimination alone is not an end-to-end
speedup claim.

Allocation refusal sends EXTENT_DENIED without a descriptor and drains the
child's failure acknowledgement before returning the original typed cache error.
The source session remains usable after this synchronized refusal. Malformed
frames, guard refusal, or uncertain completion abandon the exchange and publish
nothing. The 4096-byte bound includes failure diagnostics: an oversized diagnostic
abandons the session rather than preserving recoverability or allocating an
unbounded reply. Typed child Hung/Abandoned errors retain their types while
abandoning the exchange after a destination has been allocated. Any child failure
after destination allocation abandons the session, including FAILED/OTHER; the
same ordinary failure before allocation leaves the session usable.

Before attempting an FD handoff, the parent marks the destination as exposed.
Failure or unwinding then keeps the unique uncommitted extent in the failed
session, with its existing cache charge, until writer exit is proved. A partially
failed send can still have delivered the descriptor. Neither EOF, a closed socket,
nor successful signalling proves that the writer mapping is gone. The retained
extent is never read, committed, hole-punched or reused while ownership is
uncertain. `WorkerSession::retained_export_bytes` reports this session-held storage.

Dropping the session transfers its child and retained extents to a process-lifetime
quarantine before polling exit. A successful `Child::try_wait` exit status is the
release proof. The `Child` owner is the sole waiter; other code must not reap its
PID separately. Teardown polls for up to two seconds before signalling a child
whose ownership remains unambiguous, then polls for up to fifty more milliseconds.
Background reaping cannot signal an owner until the dropping session finishes
that grace period and explicitly permits signalling. Each polling step uses the
nonblocking child wait API. Any wait or signal error, or panic during an owner
operation, permanently stops both further waits and signals for that owner.
A PID may already have been reused, so another `Child::try_wait` cannot establish
safe recovery after ambiguity. Its storage and charge remain retained for the
process lifetime. Owners with no ambiguous operation can still be released by a
later confirmed exit. Private threaded fixtures use completed joins as their
corresponding proof.

`worker_quarantine_stats` reports retained worker/extent counts, retained extent
bytes, ambiguous owners, successful signals, wait/signal errors and confirmed exits.
The retained fields are gauges; error, signal and exit fields are cumulative totals.
`reap_worker_quarantine(limit)` retries at most 64 owners per call in round-robin
order. Connecting a new worker also retries one old owner. Reaping never releases
on a signal result, never signals during grace, and never waits or signals an
ambiguous owner again. A permanently ambiguous owner retains capacity indefinitely;
this conservative refusal is preferable
to reclaiming storage while a writer may still use it. These are runtime physical
resource observations and do not enter snapshots, logical EventHistory or cache
commitments. Retained bytes describe extent ranges, not process RSS or total
virtual backing size. The cache remains append-only and retires empty backing
segments after their ownership has ended.

The parent never accesses a destination while a successful child exchange owns
its writer mapping. The child checks offset arithmetic, slice length, exact
required length, and current descriptor backing size before mapping. That size
check cannot prevent later truncation; the coordinator keeps backing storage
alive and untruncated for the complete writer lifetime, including uncertain
completion. macOS can conservatively retain a released range's charge until its
remaining segment extents are dropped; Linux may reclaim a proved-retired range
with hole punching.

Worker framing work uses `transport.parent.<group>.<send|receive>` and
`transport.child.loop.<group>.<send|receive>`. Fixed groups are hello, setup, run,
observe, snapshot, identity, replay, export, import, telemetry and unknown. All
allocation-handshake frames remain in export. The parent knows the outstanding
request group; the child classifies a fully received request by opcode, and failed
or incomplete new requests use unknown. Parent hello accounting transfers into
the established session; child hello accounting shares its serving-loop owner.
Failed startup has no returned session report.

Each direction reports frame `attempts`, framing `completed`, framing `errors`,
actual `header_bytes`, `body_bytes` and `fd_handles`. Bytes are positive local
socket syscall results, including partial progress before failure. A complete
frame has a 16-byte header; descriptors are excluded from bytes and counted once
on positive sendmsg or when recovered from recvmsg. Truncated ancillary data counts
only recovered handles, not unobservable discarded descriptors. Mapping shared
ranges, kernel copies, network traffic and peer application acceptance are not
measured by these fields. Framing completion does not imply protocol, operation
or post-I/O guard success. Pre-I/O guard refusal records no transport attempt.
Inactive groups are omitted and counters saturate; concurrent field snapshots
are approximate. Linux interruption, EOF and deadline behavior is unchanged.
Darwin frame-continuation receives retry interrupted syscalls and reject
unexpected descriptors before classifying an empty receive as EOF.

Parent and child totals describe opposite endpoints and must not be added as
unique traffic. Child snapshots replace cached totals on successful telemetry
queries; its response encoding/send appears in a later query. Parent query I/O
is immediately visible. Failed queries preserve the last child snapshot, so a dead
child's final work may be unavailable. These diagnostics are not serialized into
snapshots, certificates, identities, retention or search policy; no protocol
format changes are involved.

Guard work is observational, per owner, and exported by bounded site. Direct
Session and UML totals use `guard.session.<site>`. A WorkerSession reports its own
checks as `guard.parent.<site>`, the child's session/core checks as
`guard.child.session.<site>`, and the serving loop as `guard.child.loop.<site>`.
Clones within one session share an owner; each worker spawned by a reused launcher
forks an empty owner, so another session's cumulative work is never copied into it.
`WorkerLauncher::telemetry_counters` separately exposes launcher construction work
under `guard.launcher`; session parent totals start with that session's actual
spawn checks. Session startup includes its fresh admission/identity measurements.
Standalone identity preflight calls outside a session and cross-process child-file
identity checks are not session guard totals. Failed construction has no returned
session from which to query counters; the low-level caller-owned measured API
retains refused admission work when that boundary needs observation.

Sites are `admission`, `session_boundary`, `launcher_before/after`,
`rpc_before/after`, `receive`, `handled`, `sent`, `publication_before/after`,
`control_before/after`, and `sdk_response`. Direct export uses four
parent sites: `export_before` before EXPORT, `extent_requested` after the first
reply (NEED_EXTENT or FAILED),
`extent_allocated` after the allocator callback before FD handoff, and
`export_completed` after completion reception. The child reverse exchange uses
`extent_before` before NEED_EXTENT, `extent_sent` after that send before waiting,
and `extent_received` after the allocation response before mapping. The serving
loop's `receive` check is its one initial check; each `sent` check also covers the
next receive.
Unclassified public session boundaries remain explicit `session_boundary` entries.
Core hook tags describe generic control/SDK boundaries; executable policy remains
in the client. Each site exports dispatch attempts, errors and host `ns`, fixture
dispatches, actual `strict_attempts`, `strict_errors`, and `strict_ns`. These strict
totals include fresh capture and recheck work. `strict_check_attempts/errors/ns`
and `strict_capture_attempts/errors/ns` expose those disjoint stages, including
checks during admission. Fixture placement counts are not real executable
measurements. Timing envelopes overlap:
strict time is inside dispatch time, so the two must not be added. Dispatch timing
stops before counter accumulation/export. A local delta
is merged after each check, never while holding an accounting lock across policy
checks or callbacks. Snapshot/replay does not rewind these host-work totals.

Linux byte fields report actual live-code/vDSO reads, sealed executable code reads,
SHA-256 input and comparison input, with admission-only full-file reads/hashes
separate. `code_page_entries` counts the code and vDSO pages checked.
Successful partial reads remain counted on refusal. ELF/proc metadata,
digest framing and cross-process child identity work are excluded. macOS exports
no Linux byte leaves, while reporting its kernel signing state, static-signing
and image-census stage attempts. A telemetry query replaces
cached child totals rather than adding them. Cumulative
`guard.child.telemetry_queries` and `guard.child.telemetry_query_failures` remain
meaningful when added across sessions; failures remain visible after later queries.
Child values are the last successfully queried totals, not a final account
of work in a dead or refusing child. Parent checks for the query are immediately
visible; child checks after its telemetry snapshot appear only on a later query.
Inactive sites, with zero dispatches, are omitted. Per-site `byte_measured_attempts` and
`byte_unavailable_attempts` count strict attempts with and without byte accounting,
respectively, so addition preserves availability coverage; fixture dispatches count
neither. These counters do not enter identities, snapshots, EventHistory,
certificates, retention budgets or search decisions.

`UmlSession`, behind the `uml` feature on Linux, runs a User-mode Linux guest
through `uml::Session`. `UmlLaunch` names the profile, initramfs, memory, boot
arguments, seed, setup budget and the progress limit after which a guest that
makes no virtual-time progress is reported as `SessionError::Hung`. Boot runs
the guest to its setup snapshot point and captures it. Each snapshot is a full
`uml::Checkpoint`; a restore is in place while the guest is paused and starts
a fresh process otherwise. The initramfs override is pinned before the first
boot; its sealed bytes define the root identity and serve every later fresh
process. Boot checks their SHA-256 against the caller's admitted initramfs
digest. The caller supplies the admitted profile and exact host processor
identity; boot checks both against the pinned profile and
processor before starting a guest. The root identity also includes the loaded
Harmony executable identity, so a checkpoint produced by another host build
cannot be imported even when the profile and guest bytes match. It includes a
length-delimited digest of the complete admitted host identity, including the
CPU-info lines UML imports at boot, and the exact boot argument list and
initramfs bytes.
An import from a different physical processor or launch
input is refused before the checkpoint store changes. Fresh restore spawns
recheck the same admitted processor and host identity. A publish exports the
checkpoint into one extent,
and an import reads it back with the session's service factory. Branches
refuse mechanical effects, because the UML backend cannot write guest memory
or inject interrupts. If handler or payload validation rejects a branch after
its checkpoint has been restored, the client abandons the guest so a caller
cannot continue from the rewound state after receiving an error. The client
also checks the active service handler around branch factory calls and cached
imports, abandoning the guest if a factory, clone, reseed, or restore changes
the live handler through shared state. `store_bytes`
is the allocated size of every held checkpoint, and telemetry reports the
count and host time of captures, restores, fresh restores and imports.

`SearchSession` is the set of calls a search worker makes, implemented by
`Session`, `WorkerSession` and `UmlSession`.

`placement::CorePool` finds the host's fastest core type within the process's
CPU affinity, cut to the whole cores of the tightest cgroup `cpu.max` quota: the `cpu_core` and `cpu_atom` lists on hybrid x86, the part
number in `MIDR_EL1` on arm64, and `cpu_capacity` or the maximum frequency to
rank the types. Linux then uses package/core identifiers and sibling lists to
put one representative of each physical core before SMT siblings, retaining
speed order within each pass. The quota applies to that order. The coordinator
is the last included physical representative, reserved before worker assignment;
it remains the same processor at every worker count. Workers use other physical
cores before sharing those cores through SMT. A single physical core with
multiple allowed threads necessarily shares that core with the coordinator.
Missing topology falls back to package/core identifiers or individual logical
CPUs; contradictory present sibling/core metadata is refused. `topology()`
reports that knowledge, packages, highest data/unified shared cache and NUMA
nodes. It never admits a CPU outside the original affinity and fastest class.
`CorePool::plan` refuses more workers than the logical pool leaves room for.
A pool of one logical CPU holds one worker and the coordinator together.
`pin_current_thread` binds
the calling thread to one core on Linux. A worker's guest runs on the thread
that owns its session, so pinning that thread keeps the guest on one core. On
macOS the pool counts the performance cores and nothing is pinned.

CPU topology metadata has private, fail-closed admission caps: 64 KiB per text
value, 4 MiB of cumulative census text, 4,096 expanded ID occurrences per list,
1,024 eligible CPUs and online NUMA nodes, and 8,388,608 cumulative parsed or
copied ID occurrences. Overlapping ranges count repeatedly before deduplication;
all range lengths and aggregate charges are checked before expansion or copying.
Sparse high identifiers remain valid within these cardinality limits. The file
reader stops at the per-value limit plus one byte and refuses oversize values;
numeric and cache-kind text share the census budget. Present metadata beyond a
cap is refused. Present malformed hybrid core-type lists are also refused;
missing files and valid empty or noneligible pools retain the existing fallback.
These limits bound host metadata processing, not guest execution or host speed.

The compressed full-LLC census fixture uses the production maximum of 1,024 CPUs
natively and 16 CPUs under Miri. Both profiles use the actual census, a compressed
range covering every CPU for the NUMA node and shared cache, and the same complete
per-CPU assertions. Native tests cover the maximum-scale admission policy; the
interpreter profile exercises memory safety of those paths without repeating the
quadratic maximum-host membership workload. Malformed, size, aggregate and refusal
fixtures remain enabled in Miri. Production caps and CI time budgets are unchanged.

Placement runs before target creation and guest population. Memory policy is
inherited from the host; affinity alone does not relocate populated pages or
guarantee a NUMA allocation. Shared snapshot extents retain their existing page
ownership and allocation policy. Topology metadata does not certify page
residency or establish an SMT/NUMA throughput improvement.

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
UML terminal settlement waits for the host process to exit and the console
collector to finish before reporting the diagnostic tail. The kernel-owned exit
status decides a panic; console text does not. Crash diagnostics report the
settled ordinary exit status whether the bridge closure or process exit was
seen first. A signaled host exit abandons the session instead of producing a
guest crash or shutdown. Host exit87, host signals and launch resource limits
return typed `SessionError::Infrastructure` errors, including wrapped restore
boot failures. After setup, guest panic86 retains its guest crash classification.
Before setup, guest exit and panic remain control errors with the actual terminal
reason; they cannot form a runnable session. A host progress timeout remains `SessionError::Hung`; malformed replies remain control
errors. Classification uses owned child status rather than diagnostic text.
Abandonment reaps the guest and settles its console before returning diagnostics.
`WorkerSession` preserves typed infrastructure errors through the current image hello
and distinct error tag; they abandon the worker just as progress timeouts do. A
worker transport disconnect is also infrastructure failure: legitimate guest exits
arrive in framed stop replies. Complete malformed frames retain control/protocol
error semantics. Native controls check an owned worker killed by SIGKILL and a
malformed frame from a peer held alive; the Faults admission guard separately
checks the resulting typed infrastructure error without requiring a test-only
public worker factory.

Durable roots use the `SearchSession` root methods, separately from the transient
cache namespace. Hardware roots remain deltas against the verified deterministic
setup. UML roots contain all nonzero guest and private-image pages, plus service
sidecars, and bind the profile, exact host processor identity, initramfs,
memory, arguments and seed. They can be
imported over a fresh launch whose host-private setup bytes differ. Cache deltas
still require their original setup identity. Neither interface interprets
workload commands.

Search sessions can publish a `SnapshotAttachment` containing an opaque domain
and payload alongside a cached snapshot. The client assigns no workload meaning
to those bytes. `publish_snapshot_attached` carries them through both direct and
process-worker exports; `import_cached_receipt` returns the target attachment,
its machine commitment, and its independently verified object commitment.
Existing attachment-free calls use an empty attachment.

The `HMSESS02` cache envelope contains checked lengths for backend metadata,
attachment domain, and payload, together with the machine commitment. The
version-two object commitment binds that machine commitment and both
length-delimited attachment fields. The VM machine commitment already binds the
admitted image identity and portable target state; UML binds its admitted root
and setup domain plus the checkpoint digest. Imports compare the object digest
against the pinned index record and verify the backend machine commitment before
accepting the imported handle. Each delta carries its complete target attachment;
resolving a chain never substitutes an ancestor's attachment. Older envelopes
are refused. UML's separate imported-root format remains distinct from ordinary
session-cache envelopes.

A duplicate cache key with different attachment evidence follows the existing
determinism contradiction path, preserving both witnesses and quarantining the
namespace. Attachments preserve existing page validation, leases, revocation,
and host-image guards. They are composition-produced provenance protected by
immutable cache ownership, not signatures against a malicious producer or a
replacement for checking the actual imported machine state.

Cache namespaces select their key semantics at construction. `Namespace::new`
uses action-prefix keys: lookup returns the longest retained prefix and a delta
parent must have a strictly shorter prefix key. `Namespace::exact` uses opaque
keys: lookup requires complete equality and a delta parent may have unrelated
key bytes. The constructors use separate digest domains, so the same namespace
parts cannot mix key modes. Exact mode retains parent ownership, namespace,
existence, and commitment checks. Entries always link to already retained parent
entries; equal duplicate publication returns the existing entry without changing
its ancestry. A conflicting duplicate retains both witness chains and
quarantines its namespace in either mode. Composition adapters using opaque path
roots must search their own certified ancestor roots when an exact key is absent.

UML imported roots also retain their source index commitment. After validating
and importing the checkpoint, the session recomputes the root-domain machine
commitment and compares it with the pinned index record before installing a
handle. Final host-image and lease checks remove a temporary handle on failure.

`Session::sdk_events_since(offset)` and `SearchSession::sdk_events_since(offset)`
return the SDK event suffix starting at the zero-based event index. Direct
sessions retain the control protocol's checked pagination; process workers carry
the offset in their SDK-events request and return only that suffix. UML skips
prior events before copying payloads. `sdk_events()` explicitly requests offset
zero. Backends using the trait's fallback fetch their complete SDK history and
slice it; callers must distinguish that correctness fallback from native suffix
transport when measuring cost.

An offset at or beyond the available event count returns an empty suffix.
Advancing a cursor beyond `u32::MAX` fails; a failed multi-page read returns no
partial suffix. A cursor is scoped to its actual snapshot and branch lineage;
this API does not authenticate a previously consumed prefix or detect rollback.
`EventHistory.count` counts ordered history commitment records and is not an SDK
offset.
Workload adapters own decoder checkpoints and lineage validation.

Explicit worker copies use `copy.parent.*` and `copy.child.loop.*` with independent
owners; internal serializer clones share their owner's counters. `encoded_payload_bytes`
counts measured `Out::bytes` payload extensions (excluding scalar field encoding
and the worker hello, which precedes the copy owner),
`status_payload_bytes` counts the successful reply-body copy into its status
wrapper, and `decoded_payload_bytes` counts owned INIT/setup/attachment/answer
payload fields, parent status stripping, and parent SDK-event payload decoding.
Each actual site counts after its copy, even if later decoding/validation fails.
Other string decoding, branch reproducer internals, buffer growth/allocator moves,
Session sidecar construction and kernel copies are excluded: this is an inventory
of these explicit sites, not all memory traffic or allocated bytes. The same
payload can legitimately appear at several distinct copying sites.

`copy.child.loop.import_mapping.*` and `export_mapping.*` report mapping attempts,
errors, successful logical `range_bytes` and actual requested `span_bytes` (which
includes alignment skip). Successful mappings preceding later failure remain
counted. They describe repeated virtual mappings, not resident bytes or reads or
copies; borrowing their slices records no copy. The parent currently creates no
such mappings. `Session` reports `copy.session.extent.*` (renamed
`copy.child.session.extent.*` through a worker) for the completed logical
`encoded_bytes` and actual output `metadata_copy_bytes` / `page_copy_bytes` at its
final extent writer. UML exports do not emit these extent-copy counters. Padding is
part of encoded length but not copied metadata. No E-byte temporary output buffer
or temporary-to-extent copy exists in direct export, and no such charge is inferred.
Sidecar/row allocations remain separate and are not estimated from output length.

`snapshot.copy.*` counts Store interning and read-into-buffer copies, forwarded
once through core telemetry. Copies from `Store::materialize` into its tempfile
mapping are excluded. `intern_calls` includes calls that later fail;
`intern_hits` counts reused equal resident payloads with no new owned copy;
`intern_copy_bytes` counts actual new owned page copies, including copies whose
builder is later discarded. Zero or unchanged writes can bypass interning.
`read_copy_bytes` counts the public Store read-into-buffer copy, while borrowed
transaction reads count zero. Generic Store totals include boot/capture/restore
work and are not labelled import-only bytes; existing import page counts still
mean merged rows, not physical copying. Store, worker and Session owners are
runtime-only, do not rewind on snapshot restore, and have no global shared-index
component. Child copies replace cached totals on query just like child transport.
Child query-response copies become visible at the next query; the parent's own
response-decoding copy is visible in the current query. These counters do not
enter StoreStats, identity, certificates, memory budgets or search policy.

Reading worker telemetry sends one guarded TELEMETRY request. Explicit
`snapshot_owned_pages` and `store_bytes` calls send separate Snapshot-group RPCs;
they are not part of the worker telemetry getter. Callers that also collect
ownership gauges must place those queries outside work-counter phase snapshots.
Parent and child counter snapshots bracket different response-publication points;
the fixed telemetry request's framing and guard work remain observable.


Accepted process-worker imports expose cumulative `cache_import.parent.*`
counters without changing the descriptor protocol or mapping work. `extent_references`
counts N chain references; `backing_occurrences` sums U distinct parent Segment
allocations per import, and `duplicate_references` sums N−U. These are allocation
identities, independent of descriptor numbers, allocator-local segment IDs, and
contents. They do not measure globally unique kernel backing objects.

`recent_backing_hits` and `recent_backing_misses` classify each current distinct
allocation against the prior per-worker window, before refreshing it in first
reference order. The window retains at most 128 weak allocation witnesses;
`recent_window_evictions` counts capacity removals. Witnesses identify the Segment
allocation shared by distinct extent owners, not each extent's Arc allocation.
Weak witnesses preserve control block identity without retaining mappings,
descriptors, extents, or page storage.
A fresh worker starts with an empty window. Hits indicate bounded recent recurrence,
not lifetime reuse; misses include first sightings and evicted allocations.

`accepted_imports` advances only after the final lease check. `measured_imports`
counts accepted layouts of at most 64 references. Larger accepted layouts advance
`unmeasured_overlimit` without changing the window or N/U totals. All counters
saturate at u64 maximum. Refused and interrupted imports contribute nothing.
These counters add no ownership RPCs, mapping deduplication, registration cache,
or admission authority. Descriptor and mapping counters still report actual work.

Explicit session shutdown reports whether the currently owned process completed
teardown. WorkerSession closes its transport and uses the same bounded grace,
signal eligibility and sole-owner exit checks as Drop. An ambiguous or still
running owner returns an error and remains quarantined with its exposed writable
extents charged. The result is latched; repeated shutdown and Drop do not poll or
signal that owner again. Background quarantine reaping retains its existing rules.
Direct in-process sessions have no external child. UML delegates to its owned
Guest teardown and records an idempotent receipt; successful group cleanup uses
the existing launcher checks, not a new guarantee about arbitrary descendants. An
abandoned UML session refuses shutdown confirmation even after removing its live
Guest; normal confirmed shutdown remains idempotent.

## Execution assurance

The version 5 worker host hello separates the combined qualification-aware INIT,
descriptor-table IMPORT and execution-endpoint protocol from every prior schema.

Sessions default to ordinary, explicitly unqualified execution. Standard
libraries remain admitted and CPU portability warnings remain advisory. Set
`SessionConfig::with_qualification` to request a closed finite composition;
`UmlLaunch::qualification` carries the same request. Both direct and inherited
worker admission validate it before creating a guest. Current whole-composition
candidates all refuse: the retained Linux cross-vendor state contradiction is
failed and UML whole-state identity is unavailable. The request is not a switch
that promotes ordinary artifacts into qualified evidence.

The [execution assurance contract](../execution-contract/README.md) names the
required artifact, engine, surface, substrate, restore and evidence bindings and
the required transformation matrix. Ordinary assurance is bound into VM image
identity, portable snapshot metadata, UML root identity and cache namespaces.
Missing/changed portable assurance metadata is refused. These bindings distinguish
an observed replay or cache equality from an exact-qualified result; they do not
claim new architectural equivalence or relax any backend admission restriction.

Endpoint whole-state/history proofs also carry the actual session's admitted
ordinary assurance identity. Workers return that identity with the measured state
and history, and decoders and endpoint collectors refuse relabelled proof metadata.
Fault history compares it with the parent receipt before extending the history.
The proof request does not supply an expected assurance value to manufacture a
reply. UML retains unavailable whole-state evidence; endpoint batching does not
promote any ordinary execution into a qualified finite composition.
