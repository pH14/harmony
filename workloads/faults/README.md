# Faults workload

`faults-workload` searches a distributed workload for bugs that only appear
when nodes die, stall, or restart at the wrong moment. The workload runs
unmodified inside one deterministic VM; every fault is enforced from inside the
guest by the platform supervisor, and the whole run is a Consonance session, so
a bug reproduces from its action list alone.

## The image contract

A workload image is an OCI image carrying one extra file, `/etc/harmony/bundle`,
in the platform supervisor's bundle format:

| line | meaning |
|---|---|
| `node <name> <argv...>` | one workload process the supervisor supervises |
| `hook <id> <argv...>` | a command the search can run at any moment |
| `setup <argv...>` | runs once, before any node starts |
| `ready <argv...>` | must pass before setup is sealed and before new hooks launch after a supervised node start |
| `workload <argv...>` | one long-lived workload driver started after initial readiness |
| `check <argv...>` | a short-lived oracle command run continuously after initial readiness |

[`prepare`](src/prepare.rs) stages that image, packs its rootfs, reads the packed
bundle for the action alphabet, and selects `/etc/harmony/bundle` for structured
supervision. The platform control
member contains the read-only execution specification and mounts the pinned
supervisor, SDK devices, and bundle path. The supervisor runs setup and
readiness commands with the resolved image credentials before it publishes the
setup point, then owns the node process groups and hook launches.

After setup, readiness probes run asynchronously while standing-fault polling
continues. The configured command decides readiness, including whether it can
operate with some nodes down. Already-running hooks continue reporting their
assertions across restarts; readiness only checks new launches. Bundles without
a readiness command keep immediate hook launches.

## Image admission

Preparation scans every executable and shared library in the exact packed
rootfs segment that the guest receives ([`admission`](src/admission.rs)). It
derives the bundle and event attestation from those same archived bytes. A
staged file changed between scanning and packing cannot make admission name
different executable or bundle bytes from the guest image.

Packaging temporarily grants host read and directory search permission while
copying staged files, then restores their original modes. The archived scan
reads those copied bytes regardless of mode, so images containing mode-zero
files and directories receive the same admission result on privileged and
unprivileged hosts. The separate image-inspection command still inspects the
staged rootfs directly.

The scan rejects:

- a writable executable segment or an executable stack;
- an ELF file that is not 64-bit little-endian x86-64 or arm64;
- a hardware entropy or CPU-identity instruction (RDRAND, RDSEED, RNDR,
  RNDRRS, RDPID, LSL) without a review in `/etc/harmony/instruction-allowlist`
  ([format](../languages/reviewed/README.md));
- an arm64 monitor-based LL/SC instruction, even if listed for review;
- an arm64 executable load that contains an allocated non-executable section;
- a review entry that matches no instruction in the image.

Native arithmetic and standard math libraries are admitted for both VM and UML
images without instruction reviews. Reciprocal/reciprocal-square-root estimates
and x87 transcendental operations may produce different result bits on different
CPU implementations. Image admission does not prove a common numerical
transition rule across CPUs. Saved CLI replay warns about changed or unknown CPU
implementations through [CPU provenance](../../cli/README.md); exact agreement
still needs evidence for the particular implementation
pair, artifacts and inputs.

For User-mode Linux, preparation rejects the scanned hardware entropy and
CPU-identity instructions even when reviewed. UML runs guest user instructions
natively, so the VM's hidden feature bits do not protect a guarded call site.
It also refuses `XBEGIN` and valid `XACQUIRE` instructions before execution,
including uninstrumented images and reviewed sites. RTM and HLE can abort for
unmodeled cache, resource and asynchronous conditions; one guest CPU does not
make their outcome deterministic. The rule applies to both fault-search
preparation and ordinary application preparation from the archived rootfs.
It does not add an instruction-review requirement for these sites in VM mode.

The entry-only rule follows Intel's
[transactional instruction definitions](https://cdrdv2-public.intel.com/671143/334569-sdm-vol-2d.pdf)
and [asynchronous-abort guidance](https://www.intel.com/content/www/us/en/developer/articles/technical/software-security-guidance/technical-documentation/intel-tsx-asynchronous-abort.html).
`XEND`, `XABORT`, `XTEST`, `XRELEASE`, `XSUSLDTRK` and `XRESLDTRK` do not start
a transaction and remain admitted. Outside a transaction, their supporting
CPUs respectively fault, perform no abort, report inactive, release normally,
or leave tracking unchanged. This is not a claim that unsupported CPUs execute
them instead of raising an instruction exception. `XACQUIRE` classification
requires a valid memory operation, including implicit-lock `XCHG` or a supported
explicit `LOCK` form; ordinary REP, PAUSE, SSE and `XRELEASE` prefixes remain
admitted. The scan still requires cooperative control flow and no generated code.

Production preparation applies the x86 xstate policy to the exact packed ELF
bytes for both VM and UML execution, including ordinary uninstrumented runs.
FXSAVE/XSAVE-family saves are refused independently of digest/address review.
Only the complete straight-line glibc eager-resolver and unused-TLSdesc shapes
receive the loader exception; the save operand, matching restore, mask/reload sequence,
predecessors, alternate entries and overlapping decodes all participate in the
check. TLSdesc and text relocations anywhere in the scanned x86 ELF closure are
refused. XGETBV requires a unique nonoverlapping straight-line proof that ECX
is zero, bounded to 32 predecessor instructions. The proof follows full-width
register copies, respects partial-register and CPUID clobbers, and permits
conditional branches only when no observed entry bypasses the proof. LOOP-family
instructions, calls and unknown operations stop the proof. Legacy resolver
FXSAVE/FXRSTOR requires its complete 16-byte-aligned, 0x240-byte frame; the
XSAVE variants retain the matching mask and 64-byte alignment rules. Restore operations remain
admitted, and native standard math is unchanged. The Linux fixture compares
compiled FXSAVE, XSAVE, selector-1, proven selector-0, restore, math and ordinary
statically linked libc programs
through both production preparation modes and the Python checker, retaining
exact ELF bytes and both decisions. The ordinary libc positive runs in VM mode;
installed glibc can contain transaction entry already refused by UML admission.

This is a static cooperative-code boundary. It does not enforce future
executable mappings, mutable files, generated code or instruction preservation
under engine WriteMemory/XorMemory effects. Those effects can change code even
when the original image was admitted. Ordinary logical XSAVE/cache identity
continues to require the documented instruction-domain assumptions; the scan
is not a runtime domain-preservation or exact-replay qualification certificate.
A retained development-host KVM probe passed static admission, decoded FXSAVE
into an RW mapping, changed the mapping to RX and executed it successfully.
That is a confirmed instruction-domain escape, not a replay-divergence witness.
Arbitrary RAM effects and dynamic execution remain outside the maintained
static contract even after the production/checker alignment.

The stub's TSC trap does not trap RDPID or LSL. RDPID returns the host CPU's
identity, and LSL can read the CPU number from the host GDT segment limit.
This admission check walks executable loads in the packed image, including
file bytes mapped before a segment's virtual start and beyond its declared
file size in the first and last executable pages. It also rejects loads whose
file offset and virtual address disagree within a page. Section headers do
not determine what the guest loader executes.
The x86 scan follows instruction streams from each mapped section and gap
across section and adjacent executable-load boundaries. It starts at each
load's declared address, restarts at direct near-branch targets in any
executable load of the same ELF, including across unmapped gaps, and rejects
overlapping executable mappings, which could combine instruction bytes from
different loads. The scan still requires
cooperative workloads: dynamically generated code and indirect transfers into
another instruction's operand bytes are outside its guarantee.
The checked-in x86 Bookworm images carry reviewed libstdc++ and libgcrypt
entropy sites, so UML refuses those images even if a particular workload
would not call them. Build a UML image without the sites before searching or
running it; the review entries remain valid for VM execution. Saved UML runs
created before this admission rule cannot be replayed or branched because their
guest initramfs has no admission evidence.

Each line of `/symbols/harmony-instrumented-events` holds a SHA-256 and an
absolute image path, and must match that file. Event actions are enabled when
this attestation passes, `/usr/lib/libvoidstar.so` exists, and `/symbols` holds
a non-empty `*.sym.tsv`. An image without an attestation still runs the other
fault actions.

The scan catches entropy and CPU-identity instructions that compilers and
assemblers emit. On x86 it decodes each allocated section inside an executable
load from its start, regardless of the section's executable flag, and scans
the gaps separately. Each decode continues through later ranges when their
virtual addresses are contiguous. This preserves instruction alignment when a
segment begins with headers or other data while finding an instruction reached
by falling through a boundary. Direct near jumps, calls, and conditional
branches also start fresh decodes at their targets, including targets within
an operand that the section-start stream treats as data. At each mapped-section
or adjacent executable-load boundary, it also decodes the first complete
instruction from every candidate
start within the preceding 14 bytes. Entropy and CPU-identity instructions
whose prefixes, operands, SIB bytes, or displacements cross a boundary become
exact-address review sites. A VM image can review such a site against its
executable digest; UML rejects native host-input sites even when reviewed.
Executable pages can contain data that resembles an instruction at a boundary,
so those bytes may need explicit VM review. Overlapping x86 executable mappings
are rejected. On arm64 it checks aligned
instruction words across the complete 4 KiB page-rounded file mapping of each
executable load, including bytes beyond the declared load size, and rejects
another load segment that aliases an executable memory page. Images with
read-only data in executable loads must link with separate code pages. A
trailing partial word with no mapped bytes to complete it cannot hold an
instruction. The LL/SC mask excludes LSE CASP and ordinary acquire/release
loads, which do not use the reservation monitor. A binary built to hide an
x86 instruction inside another instruction's operand bytes can still pass.

`harmony prepare IMAGE` prints the scan, the attestation, and the
instrumented files, and exits nonzero unless the image is a complete
instrumented target.

## Actions

Every action records its own duration in 10 ms guest ticks, from 10 ms through
10.24 seconds, and its window lasts that long ([`target`](src/target.rs)). The
search draws one duration per suffix from the adaptive duration policy, and
every action in the suffix takes it. Each action also records a coverage
quantum: the number of coverage callbacks an instrumented thread makes between
VM exits during the action's window. It is drawn log-uniform over powers of two
from 1 through 32768, and it is part of the action's archive key. Replay serves
the same quantum, so busy-loop scheduling replays along with fault timing:

| action | effect |
|---|---|
| `Wait(ticks)` | the workload runs undisturbed |
| `EventKill(node, rarity, ticks)` | an instrumented runtime kills the node at a selected event, reporting the claimed site before termination |
| `EventPark(node, edges, hold, ticks, target)` | an instrumented runtime holds the thread whose instrumented edge brings the count since arming to `edges`, for the hold, then counts again from zero until the window closes; each edge counts one over its site's visit count, and an optional target range of module offsets limits the count to edges whose site falls in it; `edges` is drawn log-uniform from 1 through `(1 << 14) - 1`, and the hold is drawn log-uniform in whole ticks from one tick through the window |
| `Kill(node, ticks)` | the node stays down for the window |
| `Pause(node, ticks)` | the node is stopped for the window, then continued |
| `Restart(node, ticks)` | the node is killed and comes back after a quarter of the window, at least one tick |
| `Hook(id, ticks)` | the supervisor runs that hook once |

Unrecorded standing-service questions stop at an explicit decision. The fault
client derives an answer from the action's configuration and resumes through a
recorded resolution before the guest sees it. Worker and UML sessions carry
the same resolution. Each branch also records a compact schedule of coverage
quantums from its action windows. The SDK selects that quantum directly, so a
small quantum does not create one answer-tape entry per callback.
When reconstructing an earlier prefix, the resolver uses that prefix's installed
service configuration for requests before its deadline. A request at or beyond
the deadline remains pending at the action snapshot. The next action's branch
installs its service configuration before resolving that request.

Before executing an uncached prefix, the client compiles its action timeline
in one checked cumulative pass. One immutable plan supplies the branch's exact
standing-service configuration, coverage schedule, local decision handler and
final deadline. Reconstruction compiles each prefix separately so earlier
branches retain their original configuration bytes. Compilation and full-prefix
history materialization remain linear in that prefix's depth; plans are not
retained across actions.

Each fault action becomes a standing-fault window on the shared
[`fault-policy`](../fault-policy) wire form. The package answers the platform supervisor's
standing poll with the windows whose half-open span contains the polling
moment, so an input is fully described by its encoded window list and one
branch installs it. A hold still running when an event-park window closes
continues after the window, so the next actions can overlap the held thread.
The park counts as a pending fault until that hold ends.

## Execution

[`consonance`](src/consonance.rs) drives one session per evaluator thread. A
search started by the `harmony` CLI runs each session in a child process
(`WorkerSession`, started as the hidden `harmony session-worker` command), so
macOS runs one VM per worker process; library callers without a launcher run
`Session` in the thread. Each portable action prefix maps to a real whole-VM
snapshot: the session branches its parent under the prefix's window list, runs
to the action's horizon deadline, and snapshots the exact stopped endpoint. A terminal stop is recorded
with its original stop and has no successor. If a continuable endpoint cannot
be snapshotted, the session is abandoned and the control diagnostic is
reported. Each session keeps only its current chain of prefix snapshots, at
most 32 past the setup snapshot. Every sealed prefix is published to the
search's shared snapshot cache (see [consonance-client](../../consonance/client/README.md)).
Publishing an action prefix already cached with a different target state
quarantines its namespace before the fault controller seals a link. A shared
integrity check invalidates the campaign, including work already running from
local cache links; it cannot count the contradiction as an ordinary execution
failure or return a completed campaign report. Existing leases, subsequent
lookups and publications, and dependent descendants cannot reuse that domain.
The error path writes `campaign-invalidation.json` with an invalidated verdict
and `determinism-witnesses/` with both exact anchor-first extent chains, their
checksums, namespace, action-prefix bytes and commitments. It also retains the
kernel, initramfs, optional root and UML profile, running host executable, and
setup configuration. Extents remain sparse relative to setup: reconstruction
must rebuild setup from those artifacts and verify the setup identity recorded
in their sidecars. These files preserve disputed cache evidence; UML whole-state
identity remains unavailable. An incomplete evidence write keeps an invalidated
marker and reports the persistence failure while retaining the typed original
error and its witnesses.
A prefix that the chain does not hold is imported from the longest cached
prefix in the shared cache, and only the remaining actions run again. Chain
bookkeeping lives in [`chain`](src/chain.rs). Without the shared cache a
missing prefix is rebuilt from the chain's longest matching link.
One memory budget covers the shared cache and every worker's local snapshot
store. It is the free memory left after a 1 GiB reserve and each worker's guest
RAM plus 512 MiB; the worker count drops until each worker also has room for a
store as large as its guest RAM. On macOS a search runs at most four workers.
Before each execution and after each sealed prefix, a worker reports its
store's resident bytes to the cache, which evicts entries to keep the total in
budget. When eviction cannot, the worker with the largest store drops its
oldest prefix snapshots one at a time until the total fits, keeping setup and
its newest prefix, and then drops back to the setup snapshot. The run prints the worker count and budget, and
`campaign-summary.json` records the cache's counters, including `store_bytes`
and `shrinks`, under `snapshot_cache`. It samples the shared index once under
`snapshot_cache_locks` for per-operation acquisition attempts, acquired/completed
counts and wait/hold nanoseconds; worker/session totals never sum this shared
owner. `stats` reads have their own site. These host-time diagnostics do not affect
budget policy or certificates.
A fault search keeps sixteen reservations per worker in its admission window.
Each action draws its own
duration, so one execution can run a hundred times longer than the median, and
the other workers keep running the jobs behind it until it is admitted.
Each worker reports its time through the campaign `telemetry`: boot, new
action runs, prefix rebuilds, and replayed actions, each in host and virtual
time. It also reports session restore, branch, seal, observation, and drop
time; the chain's exact hits, ancestor hits, misses and evictions; shared cache
imports and publications with their time, and refused publications; and the VMM's exit, guest-run, and snapshot counters under
`vmm.`. `campaign-summary.json` includes the work telemetry record. The campaign uses
`thread_work_telemetry`, which collects cumulative work without querying snapshot
ownership or store size. `thread_residency_diagnostics` explicitly samples the
live chain's entries, page bytes and observation-checkpoint charge. These typed
gauges are absent after the live session retires and are never accumulated into
work counters; unavailable ownership or arithmetic overflow makes page and total
charge unavailable. Each live point is queried once, so worker mode performs
real guarded ownership RPCs. Collect these diagnostics separately from phase
counter snapshots. Actual store-budget enforcement continues to query store
bytes and report them to the shared index independently of either telemetry API.

Work-counter reads in worker mode still perform one guarded TELEMETRY RPC. Its
framing and guard work remain observable, and parent/child snapshots occur at
different points around response publication. These reads are not atomic or free.
Counter getters do not estimate or subtract their own cost. Host timing, gauges
and physical request counts do not affect logical selection or certificates.

Optional job locality observations are fixed-size workload-owned values, separate
from logical fault observations and endpoint certificates. The origin restore
records its actual selected source: local exact/ancestor or shared import
exact/ancestor, selected/requested depths, session generation and final outcome.
A link imported during a previous job is local when subsequently reused.
Reconstruction is orthogonal to starting source; attempted/completed rebuilt
steps and archived parent-tail replay are separate counts. Lookup/import counters
separate origin, action-parent and cached-child work. Source selection preserves
the existing depth-first search: a shared exact match can precede a local ancestor.

An origin restore succeeds only after the existing backend-specific endpoint,
event-history and observation checks complete. UML retains its explicit
whole-state-unavailable contract. Failed/cutoff restores retain their failure stage
and attempted source when known, without treating a watchdog-converted Ok as
success. Session generations are checked, per-thread runtime identities paired
with the actual worker; they are unavailable after exhaustion and are not
persistent cache identities. Bookkeeping survives Live retirement in a job-local
TLS value and drains before panic recovery. It retains no snapshot or lease,
issues no telemetry or ownership RPC, and supplies no restore authority. These
measurements do not describe host NUMA page placement or change dispatch policy.

`origin.generation` identifies the selected starting session. `end_generation`
comes from an in-memory TLS-presence census at drain, so retirement through
current-state or event-history accessors is visible even when it bypasses the
normal Live-operation wrapper. A retired session does not erase its attempted
source generation. Generation values are observational, paired with the actual
worker, and grant no handle or restore authority.

Candidate-path lookup counts exclude the certified-local fast path. Parent and
cached-child source counts describe validated selections independently of the
origin summary; cached-child searches are exact. Parent sources count
action-parent selections for newly executed prefixes, not reserved archive
parents or parent-tail replay
actions. They are selection diagnostics, not separate whole-state certificates.
Import completion means the existing import/binding/install path succeeded; a later restore failure is still
reported separately. `NotAttempted` on a failed job means execution rejected the
job before origin restore; it is not a zero-cost restore measurement. The generic
256-byte bound is checked on campaign entry. Consumers must retain unavailable/
overflow and sink-discard coverage when aggregating results.

Consumers namespace worker/generation pairs by the observed invocation; thread
pools, resumed invocations and processes can restart those values. The sink's
caller-owned run identity supplies that namespace, outside logical campaign data.

The shared session watchdog follows deterministic virtual-time progress, so a
slowly advancing instrumented guest can finish a long action while one stuck
at a virtual moment still times out. Replay can apply explicit, bounded Wait
actions to obtain current quiescent check evidence; these actions and ticks
are reported separately from the recorded input actions.

A campaign never encodes the virtual-time trace, so the session is configured
to defer sparse checkpoint hashing. Each due checkpoint would otherwise hash
all of a gigabyte-class guest's RAM inside the run that reached it, starting
with the boot that reaches setup.

[`campaign`](src/campaign.rs) implements the game-neutral campaign interface
over that target, and [`archive`](src/archive.rs) supplies the endpoint key,
which captures assertion, liveness, in-flight work, event-firing state, faults
still in effect, and bucketed edge coverage. The edge-digest register sums a hash of every
(edge, hit-count bucket) pair that instrumented nodes have entered. It is
part of the holder identity, so an execution that drives any edge into a new
bucket opens a new slot inside its lifecycle place.
Workloads without the C runtime report a digest of zero. The
raw instrumented site reported by an event kill remains diagnostic evidence; it
is not archive novelty because a large instrumented binary can report a
distinct address at nearly every endpoint.

Assertions arrive as Antithesis SDK JSON records written to `/dev/harmony`.
The host keys each assertion by its `id` (the message when the id is empty)
and keeps its kind, message, and source location. An Always or
AlwaysOrUnreachable assertion evaluated false, or an Unreachable assertion that
was reached, is a violation and marks the execution as a bug. The set of
Sometimes and Reachable assertions that passed enters the archive key as a
count plus a digest of the sorted ids, so the key has no limit on how many
distinct assertions a workload declares. Every process's records feed the key.
`campaign-summary.json` lists every assertion the campaign saw under
`assertions`. A Sometimes or Reachable assertion that was declared but never
passed is a campaign failure: it appears under `never_satisfied` in both
`campaign-summary.json` and `report.json`, and the search prints one
`FAIL: assertion never satisfied` line for each.
`park_sites` in `campaign-summary.json` counts event-park landings by site.
`park_reads` counts, by landing site, the holds after which the held thread
read shared memory that another process changed during the hold.
The campaign turns these counts into draw feedback: each site with a read
weighs `1024 * (reads + 10 * r) / (landings + 10)`, at least 1, where `r` is
the campaign's reads per landing across all sites. A site with few landings
weighs close to the campaign rate, so one read in one landing counts for less
than several reads in a few dozen landings. The weights change at each
draw-table update. Once any site has a weight, half of the drawn
parks aim at a site picked by weight. An aimed park has `edges` 1 and a target
range that is the site alone or the site plus or minus `2^k` bytes for `k` from
6 through 12, each of the eight widths equally likely.
`park_thresholds` counts, for each `floor(log2(edges))`, the park actions the
guest ran at that threshold and the landings at that threshold, so the landings
per action at each threshold show which part of the drawn range a workload's
executions reach. A park re-arms after each hold, so one action can land more
than once. On
the SQLite WAL reset and etcd cases no park with a threshold of `1 << 14` or
more fired, which sets the top of the drawn range.

The generic `execution_work` counter and `report.json`
`execution_ticks` count the guest ticks requested by successfully applied actions
after setup. This logical counter is monotonic across target reset and snapshot
restore; longer waits cost more even when an endpoint is cached. The separate
`guest_horizons` diagnostic counts action evaluations that enter the guest;
cache reuse can change the count and reset clears it. Setup, prefix
reconstruction, and failed actions are outside the logical counter. Replay
continues an enabled continuous checker with waits of 10 ms through 40.96 s,
doubling only while its completed generation is stale or faults remain pending.
`actions_applied` remains the recorded input prefix, while `settle_actions` and
`settle_ticks` account for that deterministic validation tail.

## macOS

Hypervisor.framework allows one virtual machine per process, so a macOS run
takes one worker per process. A worker releases its target when it finishes so
the next one can boot a virtual machine in that process.

## User-mode Linux

`--backend uml` with `runner.options.uml_profile = "PROFILE"` in the TOML
recipe runs the same search on a
User-mode Linux guest (`UmlSession` in
[consonance-client](../../consonance/client/README.md)) as an ordinary user on
any Linux host, with no KVM. The profile is verified at start, its kernel
replaces `runner.options.kernel`, and each session runs in the search thread. The profile
identity, the loaded Harmony executable identity, and a digest of the complete
host identity, including the physical processor and CPU-info lines UML imports
at boot, join the
execution identity and the workload identity, and the campaign stream and
snapshot checkpoint get UML formats of their own, so a UML run never reuses
VM state. A profile changed after workload admission is rejected before guest
boot. Saved searches and command runs also bind the profile loaded for guest
execution to the recording's verified profile identity and require its
executable digest to match the saved kernel. The staged initramfs is checked
against the requested image, then the sealed bytes are checked
again before boot, so the identity names the image the session actually runs.
The UML identity hashes kernel arguments with their
lengths, preserving the boundaries used at boot when a live session or cache
entry is reused. Search admits one processor from the selected coordinator
placement for the whole campaign.
Host worker threads still run on their assigned cores, while every UML guest
child is pinned to the admitted processor; this prevents guest-visible CPUID
or MIDR differences from entering snapshots passed between workers, at the
cost of contention among concurrent guests. Standalone debug and replay use
the same coordinator selection, so they do not switch guest processors when
they leave the search worker pool. Each snapshot
is a full guest image of about the guest's RAM, so a
worker's store fills its budget faster than under KVM.

## Running it

Put explicit guest artifacts in the runner table of `harmony.toml`:

```toml
[runner]
kind = "consonance"
backend = "kvm"
[runner.options]
kernel = "vmlinux"
base_initramfs = "initramfs.cpio.gz"
ram_mib = 1024
```

```sh
harmony search IMAGE.oci --config harmony.toml --seed 1 --executions 100000 --out run/
harmony debug replay run/ --finding 1 --repeat 10 --out confirm/
```

For UML, select `runner.backend = "uml"` and set `runner.options.uml_profile`
instead of `kernel`. Workload knobs, supervision and interventions belong in
`workload.options`; see the [CLI recipe](../../cli/README.md).

Both modes write `report.json` ([`package`](src/package.rs)) with the pinned
image and kernel hashes, the execution identity, the run bounds, and
either the bugs found or the replay outcomes.

New replay outcomes encode the engine's 32-byte state digest directly as
lowercase hex and mark it with `state_hash_encoding: "engine_digest"`.
UML outcomes instead have an empty `state_hash` and
`state_hash_encoding: "unavailable"`: their guest memory has no canonical
cross-process state digest. Search still confirms a bug when the fresh replay
reproduces its recorded assertion or stop evidence. `harmony debug replay`
records the new observations but refuses to call two UML outcomes an exact
reproduction without a full-state identity.
Reports written by earlier versions omit that marker and contain SHA-256 of
the digest; readers default a missing marker to the legacy interpretation.
Earlier UML `engine_digest` values covered only bridge state and cannot prove
full-state replay equivalence.

The session records `watchdog_cutoffs` when a guest action or parent
reconstruction exceeds its host progress bound, including fresh and in-place
UML restores. Typed session infrastructure errors and abandoned sessions carry
an independent `infrastructure_failure` flag; they do not increment the timeout
counter. Neither cause is guest bug evidence. A campaign treats either cause as
infrastructure incompleteness:
the affected reservation returns an error before admission, result certification,
selection accounting, mixture updates or duration feedback. If a suffix already
completed some actions, none of that job's actions or candidates are admitted.
Bootstrap, branch-prefix replay and shared-cache imports preserve the same
infrastructure-failure classification.
The campaign stops with an explicit incomplete error instead of writing a
completed report. Previously admitted stream records are flushed and existing
search checkpoints remain available; the incomplete stream has no successful
completion record. A later search can resume an earlier complete checkpoint.
The recorded `host_failure_policy = host_incomplete_v2` binds this behavior;
streams and checkpoints with an absent or different policy are refused, including
the earlier watchdog-only policy. The execution and endpoint-certificate identities
also advance because observations now encode the distinct infrastructure flag. The
combined replay endpoint uses certificate domain v3, binding ordered event history
and infrastructure-marked observations under one schema. Reading ordered history
preserves typed host failures and rejects a known abandoned matching session as
incomplete; missing or mismatched session identities remain ordinary errors. The
shared hash helper retains the actual error even when that call retires the
session: a timeout stays a timeout and a malformed reply stays an ordinary error.
A successful hash from a session retired during the call cannot certify a
continuation. These checks also cover receipt reconstruction and imported roots.
Execution identity v10 separates the combined contract from the earlier receipt-only
identity. Ordinary
guest terminal outcomes retain their existing meaning. An outer CLI timeout is
also an infrastructure failure. Completed reports still expose
`execution_failures` and `watchdog_cutoffs`; a host cutoff cannot contribute to
either counter in a successfully completed campaign. A supervisor
runtime error has a separate SDK status and cannot turn a PID 1 exit into bug
evidence.

The searcher learns the duration of waits and instrumented event holds from
admitted campaign outcomes and logical execution cost. It continues sampling
short and long logarithmic durations while favoring durations whose own action
recently produced useful work. The adapter groups this feedback by node liveness
and whether hooks, workload, checks, or event faults have progressed. Site
identities and workload-specific concepts do not enter the duration policy.
Choices and feedback state are recorded in the campaign stream; input replay
executes the recorded durations directly.
The archive key's 128-bit assertion digest is a decimal string in JSON
campaign streams, archive reports, and progress records. This lets the tagged
job and skip records decode. The binary campaign result hash and checkpoints
retain the native 128-bit encoding.

Each campaign job's recorded result digest binds its logical result to a tagged
final endpoint certificate. The v2 result domain also binds versioned ordered-history
records (v2 includes completed semantic service exchanges) whenever a KVM whole-state endpoint is available, including terminal and
zero-action jobs. Failed or unavailable endpoints explicitly carry no history;
that absence cannot stand in for a whole-state endpoint.

Prepared execution can be admitted independently of image staging through
`prepare::admit_prepared`. It verifies the actual packed rootfs, the mode's image
admission and the selected supervisor bundle, then derives the instrumented-event
vocabulary from that admitted archive. A caller-supplied vocabulary or frontend
readiness result cannot replace those checks. Generic prepared execution records
and canonical OCI control validation remain owned by `oci-support`.

Every runnable snapshot carries a persistent, shared action history. Each node
binds its parent history, action, actual endpoint evidence, action-local
observation commitment, and observation boundary moments. KVM endpoint evidence
includes the whole-state certificate and cumulative ordered normalized-event
count and commitment; UML explicitly records that whole-state
evidence is unavailable while still committing observations. Nodes are captured
from stopped execution before their snapshots are published. Candidate clones
share nodes, and snapshot/result identities hash their history roots rather than
serializing every ancestor.

A retained endpoint restores directly after comparing its independently captured
history receipt with the requested history. This requires one endpoint replay and
whole-state hash on KVM, regardless of action depth. An imported endpoint receives
its receipt from the integrity-checked cache object, never from the caller's
expected certificates. Missing suffixes execute and check each actual prefix
before the next action or publication, so an intermediate divergence cannot be
hidden by a converged final endpoint. Snapshot standalone capture still reads
fresh live evidence; no whole-state digest is cached across operations.
Parent replay compares every archived prefix certificate with the actual receipt
captured by that same replayed action, without hashing the endpoint a second time.
The campaign still checks integrity and the typed watchdog-cutoff check before
accepting the comparison.
Cached-child hits do not materialize the action prefix. New execution still
materializes the actions required by branch configuration and window scheduling;
that work scales with prefix length. Missing-prefix checks traverse the expected
suffix once and compare borrowed receipts rather than repeatedly walking ancestors.

SDK observation decoding keeps an actual-session cursor separate from the normalized
ordered-event commitment: its offset counts every transported SDK tuple, including
ignored events. Successful suffix batches update cumulative registers, assertion
generations and hidden check passes atomically. Each action retains only parks
and reads strictly after its actual parent's observation moment. Empty suffixes
preserve the last tuple moment and repeated observations preserve action evidence.
Branch and replay install a checkpoint from the selected actual captured Link;
rollback, new sessions and failures cannot reuse another lineage's cursor.

Captured observation attachments use `fault-observations-v1`, included in the exact
cache namespace. Imported decoder checkpoints remain untrusted. When a pending
checkpoint is retained, the normal required machine replay or branch reconstructs
the full actual SDK history and compares all decoder state, including hidden maps
and offset. If pressure has discarded the pending checkpoint, restoration instead
reconstructs actual decoder state and validates the captured receipt without using
the imported decoder. Missing local checkpoints use that same reconstruction path.
First import and eviction fallback add full-history fetch and decode work, but no
additional machine restore.
Expected campaign snapshots and root payloads provide no decoder authority; fresh
root imports start from offset zero. Whole-state and ordered-event certificates
retain their existing formats and fresh integrity checks.

Action execution uses Consonance's generic owned endpoint transaction. A retained
validated parent decoder checkpoint supplies the exact SDK cursor, allowing one
Execute exchange to branch, run, capture and return the first bounded suffix
page. The request preserves the legacy run's assertion stop mask. Missing or untrusted checkpoints keep the existing selected-parent branch
and full-history verification before the guest runs. Every returned page retains
its request/stop/cursor binding; the workload decodes all pages and checks SDK
infrastructure status before Commit transfers the provisional snapshot and its
separate whole-state and ordered-history facts. A decode failure abandons the
owned session. A capture or proof failure releases all session-owned handles,
including the selected parent, without publishing an uncertified child.

Service decisions remain workload decisions. Before the deadline the adapter
answers with its current action handler and explicitly resumes. At or after the
deadline it finalizes the returned stopped question without answering or running
again. The Consonance receipt keeps the actual Decision; Faults alone projects
that observation as Deadline and leaves the pending question in the snapshot for
the next action's service windows. UML explicitly omits whole-state/history
proofs, and uncaptured terminal cuts carry unavailable evidence. The common
worker path takes Execute plus Commit, with extra bounded Page exchanges only
when needed and a FinalizeDecision exchange at that rare boundary. Inner control
and SDK guards remain active. Run host time now includes the whole endpoint
exchange and decoding; combined branch/seal counts remain operation counts,
while their separate host-time buckets cover only unbatched operations.

Optional Link checkpoints have an internal 8 MiB retention cap, including for
sessions without a shared index. This is a performance policy, not a logical
retention setting. Under shared-cache pressure they are discarded before certified
VM links. Restoring an evicted checkpoint enforces the cap and reports its charge
without moving the selected VM Link. `report_store` includes session storage plus
unique retained decoder allocations and checkpoint metadata. If a session does not
expose storage bytes, the report still includes the optional checkpoint charge.
Shared Arc state is charged once by physical identity; map storage uses a
conservative per-entry bound,
with owned string/vector capacities and Arc headers counted. Cached charges are
computed at installation, not by rescanning maps on budget queries. Current working
decoder and temporary decoding/accounting scratch remain target-private, as the
previous full-history decoder scratch did; retained checkpoints sharing that state
are still charged. Dropping optional checkpoints never changes serialized attachment
bytes, actual receipts or leases.

`observe.events` and `observe.payload_bytes` count successful suffix fetches;
`observe.decoded_events` counts successful append transitions, including ignored
records. Payload bytes exclude transport framing. `session.observe_decode` and
`session.observe_projection` separate successful decode/infrastructure checks from
capture projection. Full actual reconstruction at restore is reported separately
as `session.observe_checkpoint_rebuild` and `observe.checkpoint_rebuild_*`, for both
imports and local eviction fallback. Fetch counts can include batches subsequently
rejected; failed decode/infrastructure attempts do not contribute completed timing
samples. Checkpoint rebuild timing counts completed fetch/decode/infrastructure
checks even when a subsequent decoder comparison or receipt-binding check rejects
the reconstructed checkpoint. `observe.checkpoint_hits`,
`observe.checkpoint_evictions` and `observe.full_rebuilds` distinguish reuse and
fallback. Explicit residency diagnostics retain the VM-page meaning of
`resident_bytes`; `observation_checkpoint_bytes` is the optional pool charge and
`charged_bytes` combines both live gauges.

Fault caches use exact fixed-width action-path roots. The cache object commitment
binds both the admitted machine target and opaque captured-history attachment;
different histories for one action path quarantine the namespace even if final
machine states agree. Page, inherited-page, omitted-page, host-image and lease
checks remain enabled. This removes repeated historical VM work, not the data
integrity scans required by those checks.
Child publication selects the lease of the actual restored parent point, including
when another sibling remains resident. Runnable, terminal and interrupted-rebuild
observations all exclude park and read evidence at or before the previous endpoint.

Checkpoint histories use a validated flat encoding with explicit versions and
iterative destruction. Restore canonicalizes validated expected histories into
actual shared node allocations before rebuilding the memory ledger. Bootstrap
imports use the admitted live genesis allocation. Before archive installation, a
typed retention index normalizes a candidate's new tail against currently
budget-owned nodes. Independent workers therefore
share actual prefix allocations in both live execution and checkpoint resume.
Index registration and pruning follow the ledger's first retain and final
release, and index metadata is charged. Runtime accounting acquires only newly
unique history nodes and releases them at physical snapshot removal,
including inactive snapshots still needed by work in flight. Expected-history
canonicalization does not grant captured execution authority. Branch roots use
`HMROOT03` to preserve their independently pinned semantic object commitments.
Old history, snapshot, stream, cache and root formats are refused. Missing,
malformed, or conflicting certificates stop the campaign.

The replay certificate binds the v3 ordered-history format independently of
logical endpoint state. An intermediate event change alters the certificate even
when endpoint, observations, event count and final virtual time agree. Fresh setup
preserves the boot event prefix. Replay, branch, portable import and cache import
restore the captured prefix; snapshot capture and diagnostic segmentation add no
semantic events. Snapshot and schedule IDs never enter that commitment. Missing
KVM history cannot be replaced by an empty commitment. UML carries no ordered
normalized-event commitment and remains uncertified for whole state.

The VM stream/snapshot formats are v11/v9 and UML formats v8/v5. The action policy
is `fault-action-duration-coverage-v5-event-history`; older contracts are refused.

Restoring a source snapshot compares its independently captured receipt and
reconstructs the SDK observation before using that saved observation, including
the current action's park and read evidence. Tree import still rebuilds each
imported action and checks source-bound checkpoint identities. A source entry
digest commits its history root and ordered endpoint history. Reconstructed
certificates without a source-bound digest describe the current executor; they
do not establish the source run's intermediate states.

Campaign suffix execution and whole-tree import capture each runnable candidate
and certificate in one bounded operation with one whole-state hash. Import keeps
every action-prefix certificate and reuses its final captured snapshot. Starting
a search from an explicit action prefix captures the final replayed action in
that same operation; an empty prefix obtains fresh setup evidence. Integrity and
typed incomplete-attempt checks run before captured evidence is accepted. Terminal,
failed or watchdog-interrupted prefixes cannot return an earlier successful
snapshot. Source identity checks and every intermediate imported certificate remain
mandatory; candidate construction still clones the owned observation projection.
Parent reconstruction inside campaign jobs verifies every archived prefix through
a fresh endpoint check. Standalone snapshot capture hashes fresh live state. A
healthy job whose last action is runnable reuses that candidate's hash for its
final certificate; terminal and zero-action jobs obtain fresh final endpoint and
ordered-history evidence. Missing, foreign, abandoned or
unhashable sessions cannot supply certified KVM endpoints. Failed KVM jobs use an
explicit whole-state-unavailable tag while preserving their checked logical
observations; UML has a separate whole-state-unavailable tag. Whole-state hashes
still scan RAM at fresh endpoint capture and replay of missing suffixes, and cache
admission still verifies all required snapshot pages.

Instrumentation actions become available automatically when the staged image
contains the runtime bridge, nonempty event symbols, and an executable whose
hash matches its instrumentation attestation. A runtime hello identifies the
ready nodes in each incarnation; event faults select only those nodes, so mixed
instrumented and uninstrumented bundles share the same search policy. A step
drawn from the retained-input table is held to the same rule and falls through
to the alphabet when its node is not ready, so the readiness a run recorded
bounds every draw rather than the alphabet alone. The adapter supplies that
alphabet and the duration each drawn action carries; the searcher owns the suffix draw and the
retained-input table. The continuous client and oracle remain image-owned
commands; the oracle decides when its observations are conclusive, including
when some nodes are down.

Every replay run boots a session no earlier run has touched, so no snapshot
another run cached can stand in for guest execution: each run reaches the
sealed setup point and executes the recorded actions itself. Each run records
the actions it applied beside the horizons it ran in the guest, and the two are
equal when nothing came from a cache. A search replays every bug it records the
same way, and reports the bug as confirmed only when the replay reproduced the
evidence the campaign saw: the assertions it violated, or the same stop when
the stop was the only evidence. `bug_found` and `first_bug_execution` come from
the confirmed bugs, so a hit that no replay reproduced is reported and does not
count as a rediscovery.

Search also writes
`campaign-summary.json`, `stream.jsonl`, `progress.jsonl`,
`first-bug-input.json`, and one `bug-N.json` per recorded bug
([`report`](src/report.rs)). Each `bug-N.json` carries the action list and the
encoded window list used by the guest, including windows inherited from a
branch root. At completion, `first-bug-input.json` holds the first confirmed
finding's actions when one exists; otherwise it holds the first recorded
candidate's actions.

`FaultArchiveKey` has a place, a progress, and a holder identity. The place is
the passed assertion set and every lifecycle count except liveness, plus two
flags for faults still in effect when the state is saved: a thread held by an
event park on any node, and an event kill armed on any node. Liveness and the
edge digest are the holder identity inside the place. The progress is the
number of Sometimes and Reachable assertions the state has passed. The
selector's tiers rank states by that count and draw most parents from the
states that passed the most. Places inside a tier rank by their draw counts.

The Consonance backend needs Linux and KVM, or HVF on macOS. The UML backend
needs Linux. The action model, the bundle
parser, the archive key, the image preparation and the report shapes are
portable and tested everywhere.

Replay summaries include completed-check provenance automatically for bundles
with a continuous `check`. The supervisor records the check run, its pid, and
the disturbance generations at its start and completion. The host adds the
Sometimes and Reachable assertions that pid passed during the run, and the
pending process faults. A case can require evidence from a successful check that
started and finished in the final generation with no outstanding fault.
Assertions passed anywhere else remain exploration evidence and cannot establish
this recovery condition.
Bundles that use drawn hooks have `check: null`; their evidence comes from those
hooks instead.

Replay summaries count event-kill and event-park fires, which shows whether a
recorded input's event actions ran. The settlement wait after an input keeps
the last action's coverage quantum.

## CLI investigations

The CLI supplies an optional supervisor bundle generated from TOML. Preparation
mounts it as a read-only external input at `/etc/harmony/cli.bundle`; it never
rewrites the source image. The prepared initramfs is retained by the CLI along
with the kernel, vocabulary and execution configuration.

`package::SearchStart` selects genesis, a recorded action prefix, or a whole
search checkpoint. Prefix origins replay before capturing a root. Findings
retain that prefix, so confirmation still executes the complete history from
boot. Search checkpoints persist workload evidence as well as the generic
coordinator state. The package writes periodic checkpoints every 100 admissions
and a final checkpoint at shutdown. A CLI-selected checkpoint carries its
journal digest into the searcher, which rejects a file swapped after selection.

Fresh scenario executions retain `ReplaySummary::timeline`: the observation,
recorded action including coverage quantum, and console evidence at each action
boundary. Recovery-check waits are labeled separately. Both guest backends return a bounded 64 KiB console tail, which can
overlap adjacent captures. Console evidence is diagnostic and does not enter
archive identity or scheduling. Branch previews stop at their exact action
prefix without adding settlement actions.

UML boots the shared `/usr/lib/harmony/init` entrypoint directly. This keeps
application output on UML’s console instead of passing through the hardware
arm64 MMIO console wrapper.

## Investigation roots

The package can prepare a recorded prefix for guest debugging and export the
resulting state as a durable root. A root binds the prepared boot identity to a
cache extent containing pages and device/service sidecars, plus the standing
fault windows still active at its snapshot moment. Import validates that
identity and the extent; root bytes also contribute to the workload identity.
The CLI verifies and loads the saved root once before starting a search, then
passes those bytes to every worker. Later action prefixes/checkpoints
are relative to it. A continuation retains inherited standing windows, including
an unfired event kill armed before the root, while it adds its new action windows.
Commands and interactive input are diagnostic evidence, not
an instruction to execute those commands again during restoration.

The CLI exposes this through `branch --exec`, `branch --exec-file` and
`branch --shell`. `search --from BRANCH` restores the saved guest state.

Campaign coordinator bootstrap and checkpoint resume explicitly finish their
thread-local session before releasing execution-worker readiness. This boundary
also runs after bootstrap construction or replay errors and on unwinding. It
removes LIVE before teardown, so a process exit need not run main-thread TLS
cleanup. Unconfirmed owned-session exit fails an otherwise successful bootstrap;
a prior typed execution error remains primary, with a secondary cleanup diagnostic.
Linux targets continue reusing their thread-local session between worker jobs.
Target Drop is not a universal session-finalization boundary.

Final cumulative telemetry is captured once before explicit session shutdown and
merged into retired counters without a post-close RPC. It excludes teardown work;
telemetry failure does not prevent the owned shutdown attempt. Bootstrap scope
acquisition refuses nesting atomically, preserving the outer scope's ownership.

The generic searcher's standalone full and prefix replay mechanism owns the
coordinator thread's session for the whole operation and finalizes it on every
result. macOS target Drop defers to that active scope, preserving session reuse
inside it. The faults CLI's own replay commands use FaultTarget directly and do
not enter this generic replay scope.

The standing service declares inactive debug questions local to the installed
deterministic handler. The attached debug handler declares them external even
when no message is supplied. The supervisor still makes each actual debug SDK
request: attachment after a standing reply is delivered at the same next debug
request, without a dormant flag or an extra tick. Local answering removes only
an external Decision/resolution exchange; guest doorbells and mandatory guards
remain. Both handlers decode debug Status before answering. Rejecting malformed
inactive Status is an intentional tightening: the prior standing handler returned
empty bytes without decoding it. Standing and debug identities are respectively
faults-standing-coverage-v3 and faults-debug-terminal-v2.

The generic local-answer capability requires unchanged captured handler state;
the inactive standing debug response satisfies it. Stateful local responses are
rejected before publication. Active debug remains on the external path.

Coordinator measurement can read `coordinator_retired_work_telemetry` only when
the calling thread has no LIVE session. The accessor rejects before any session
query and returns an owned copy of retired counters. This supports bootstrap
attribution after explicit coordinator finalization without creating a session,
querying a closed child, or measuring shared cache counters twice. It does not
finalize a session or imply teardown success; the lifecycle receipt remains
independent. Ordinary thread work and residency diagnostics are unchanged.

Thread work reports add final controller counters across retired LIVE sessions.
For SDK attribution, `vmm.sdk_dispatch.event_shape_slots` therefore sums
per-session filled-slot memberships and can exceed 32. Equal shape keys from
different sessions have their values added, so this number need not equal the
number of distinct named rows in the thread report. The 32-row witness cap
applies per producer table; the accumulated thread report can retain keys from
multiple sessions and has no global 32-row bound. Neither this field nor the
shape rows report current residency or enter archive memory policy.
Fault endpoint certificates bind the ordinary, unqualified execution-assurance
identity as well as state, event history and observations. History receipts keep
and validate that identity. A KVM whole-state certificate is evidence for the
observed replay, and cannot be promoted into CPU portability qualification;
`HistoryReceipt::require_qualified` refuses unsupported finite compositions.
UML keeps `UmlWholeStateUnavailable` and has no whole-state certificate. See the
[execution assurance contract](../../consonance/execution-contract/README.md).

Execution-endpoint proofs carry the actual session's admitted assurance identity
with its measured state and ordered history. The history consumer compares that
returned identity with the parent receipt before changing observations or
extending the receipt. A relabelled endpoint proof refuses even when its other
binding fields are unchanged. The combined worker protocol requires host hello5;
both incompatible hello4 schemas and all older versions are refused.
