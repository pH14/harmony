<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# vmm-core

`vmm-core` is the deterministic VMM above the `vmm-backend::Backend` trait. It
owns the run loop, guest RAM, virtual-time advancement, entropy, device
dispatch, hypercall/control handling, snapshot and branch operations, and
state hashing. Host hypervisor calls stay behind the backend trait; concrete
backend and architecture pairs are selected by the vendor composition roots.

Native AArch64 Linux and macOS builds enable RustCrypto's runtime-dispatched
SHA-256 backend for state hashes and portable snapshot digests. Hosts with SHA2
instructions use them; hosts without them retain the software implementation.
Miri builds retain the software implementation. Hash inputs, digest bytes,
guest CPU policy, and snapshot formats are independent of this host choice.
The dependency feature also applies to other SHA-256 consumers in the same
Cargo dependency graph.

## Run loop

`Vmm::run` repeatedly obtains one backend exit, classifies it through the
architecture vendor, advances virtual time by the assigned integer duration,
dispatches devices and protocol services, and completes any pending backend
operation. Timer deadlines are applied at exit boundaries. An idle guest can
advance to the next deterministic deadline through the same clock; no host
clock is consulted. A guest idling in its polling loop writes the idle register
(arm64) or idle port (x86), which advances the clock the same way as a halt.

The run loop also times each backend run and each exit it services, keyed by
exit reason, address or port, and direction. `ControlServer::host_telemetry`
adds snapshot seal, restore, export and import time and page counts, including
those of VMs the server has already replaced. These are host wall-clock
measurements for performance accounting; they never enter virtual time, state
hashes, or snapshots.

Guest RAM is owned by `Vmm` for the lifetime of the backend. The canonical state
fingerprint and snapshot machinery cover guest memory, vCPU state, device state,
timer state, virtual time, entropy, control state, and protocol state. Vendor
fingerprint encodings cover the complete state records used for restore, while
the complete portable artifact digest covers the same persisted bytes. The
VMST uses the current version 7 wire format; a present `xsave_restore_bv`
intentionally changes the VCPU identity.
Dirty-page drains and resets distinguish unsupported tracking from backend
errors. Unsupported tracking permits a full capture; an operational error
retires the live VMM and fails the snapshot instead of publishing partial state.

Snapshots can be restored into a copy-on-write memory mapping. In-place restore
combines the snapshot difference and the guest dirty set before loading page
contents. The control server holds a store reference on the image the VM last
sealed or restored, so dropping that snapshot's handle keeps the difference
small instead of forcing a full-image copy. The resulting sorted plan borrows integrity-checked store pages and
copies each selected page directly into guest RAM. Page-write validation sorts
small address/reference records, rejects duplicates and invalid ranges before
any write, then coalesces instruction-cache invalidation ranges in the same pass
that copies the pages. It does not sort or allocate temporary page payloads. Portable format
6 preserves pending SDK stops, unanswered service requests, response sequences,
pending host effects and reseeds, the recorded input prefix, schedule failure,
and command nonce. Replay restores these without reseeding or reapplying consumed
inputs; an explicit branch selects a new plan and retains the command nonce.
Control restores borrow the retained SDK snapshot. Replay restores its service
handler and recorded environment once during preparation, before committing CPU
or RAM changes; a preparation failure leaves the live execution intact. The
prepared environment is moved into the restored VM, then its event prefix,
pending stop, pending snapshot flag, and coverage thresholds are restored from
the borrowed snapshot. Branches install their new environment plan while
preserving the same captured SDK metadata. The stored snapshot remains owned by
the server and reusable; the restored VM owns the state it can mutate.
Whole-state hashes include this control state and the recorded prefix used for
duplicate-input rejection. Writers always emit the current format, and readers
reject older envelopes. Complete reads allocate incrementally from received
bytes, and sparse section lengths are bounded by the supplied input.
Whole-VM capture preserves pending SDK stops and is side-effect-free for a
pending pvclock registration, carrying its GPA, `armed = false` state, and page
bytes so the next handshake resumes from the same state.
`compare_portable_execution_state` strictly validates two complete artifacts,
then compares their persisted bytes while ignoring only the stored
`trace_events` and `trace_schedules` counters and independently checked envelope
digests. It reports those counters for diagnostics; they are not restore inputs,
and the helper does not change live trace scheduling or establish backend
admissibility or future execution equivalence.
Pvclock-bearing device records explicitly preserve the registered page GPA,
registration capability, and pending-versus-armed handshake state. The current
x86 device format is version 6 and the current arm64 device format is version
13; each uses presence fields so optional devices retain their state without
selecting historical layouts.

The architecture-neutral engine record preserves terminal reasons and deferred
SDK reentry state in the current VM-state container. A terminal restore does not
enter the guest again.

X86 CPU capture retains SREGS2 flags and cached PAE PDPTRs, plus debug-register
flags, in the current VM-state records. Cached PDPTRs are distinct from the
current PDPT contents in guest RAM and must survive restore without reloading
them from that memory. Every standard-format XSAVE capture retains the original
`XSTATE_BV` in the tag-15 record, whether or not canonicalization changes the
x87/SSE init-state bits. The value is validated before restore. It is included in
both the vCPU identity and the complete VMST identity through the logical
projection described below. Short or compacted images may omit that optional
provenance field. A
matching fingerprint is not a proof of whole-guest future equivalence; focused
guest-byte coverage remains required.

Hardware continuation coverage depends on the backend and paging mode. AMD
default NPT has an unresolved legacy 32-bit PAE capture divergence
([#314](https://github.com/pH14/harmony/issues/314)); unchanged stopped records
alone do not prove an unchanged guest future. The separate same-seed XSAVE
divergence remains tracked in [#307](https://github.com/pH14/harmony/issues/307).

## Architecture boundary

The engine uses only common exits, guest-physical addresses, bytes, and typed
vendor traits. `vendor/x86` supplies the x86 CPU policy, loaders, device
dispatch, and records. `vendor/arm64` supplies the arm64 Image/DTB boot path,
board devices, policy, and records. The arm64 vendor is also used to exercise
the additive architecture seam on portable mocks and QEMU.

Boot does not require a particular host CPU model, stepping, or microcode.
Each architecture supplies a guest machine policy; the backend supplies the
required virtualization capabilities. The x86 runtime boots controlled Linux on stock KVM; instruction interception
patches, Multiboot payloads, and the legacy acceptance runner have been retired.
The x86 policy and snapshot compatibility
rules are documented in [contracts/x86](contracts/x86/README.md).

`boot_linux_nested_host_virtual_time` composes the separate experimental x86
nested-host contract with production `KvmBackend`. It selects KVM-supported
Intel VMX or AMD SVM and binds the vendor's exposed capabilities into snapshot
contract identity. SVM presents AuthenticAMD, revision 1, the supported ASID
count and only NPT, NRIPS, VMCB clean bits, flush-by-ASID and decode assists.
VM_CR and VM_HSAVE_PA use native KVM handling and are saved with all other
stateful MSRs. HWCR reads return the fixed P0-frequency bit for Linux's
invariant-TSC check. SYSCFG reads return zero for AMD's fixed-MTRR boot check,
with DRAM-range modification and memory encryption disabled; writes to either
fixed MSR are rejected. The matching kernel includes both
vendor backends. Nested state is carried through CPU records,
VMST tag 16, raw and component identities, whole-state hashes, and portable
artifacts. Publication requires L1 outside L2 guest mode; the inner VM remains
allocated and VMX remains enabled. `nested-driver` qualifies repeated and cold
outer restores at SDK lifecycle boundaries after inner KVM_RUN has returned.
The ignored `nested_restore_preserves_unsynchronized_vmcs_fields` test uses the
direct nested-host fixture's `harmony_nested_cache_check` mode. Its minimal L2
runner leaves segment fields unread after VM exit, then direct outer restore
must preserve the saved nested payload. L2 counts its exits in DS, and the
restored fixture must run to its last exit, so host-cached L2 segment state
from the detour fails the test. This isolates host VMCS and VMCB cache state
from the production driver's segment-state capture.
The ignored `interrupt_raised_before_nested_entry_reaches_the_nested_host` test
runs the same fixture and raises vector 0xFF each time L1 reads DEBUGCTL with
interrupts disabled before it enters L2. KVM can report an open interrupt
window while that entry is pending; the backend must hold the vector until L1
leaves guest mode, so L1 receives it and L2 never does.
The ignored `restore_before_nested_operation_onto_a_nested_host` test restores
a cut taken before L1 enabled VMX or SVM onto the same vCPU after it did, then
runs the restored guest through the fixture again.
The boxed bringup entry point uses the same composition and capability capture
for the client's dynamically dispatched session backend.

`portable_snapshot::logical_x86_sparse_sidecar` produces a non-mutating identity
projection for recorded nested campaigns. It validates the complete sidecar and
CPU records and applies the existing logical XSAVE restore-bitmap rule: presence
bits for init-valued x87/SSE components normalize, while active components remain
bound. Every other sidecar field remains encoded. The original portable bytes
retain the host restore bitmap and remain the input used for restore.

The non-default `harmony_omit_nested_state` compiler configuration is the qualification negative
control. Snapshot publication replaces captured VMX state with a valid inactive
header; for enabled SVM it discards the saved GIF flag. Raw CPU observations
still capture the real state. Uninterrupted and capture-only cases must pass.
The first restored VMX continuation or SVM control-state readback must fail.
Production builds, including `--all-features`, retain the complete state.
The control requires the explicit compiler flag
`RUSTFLAGS="--cfg harmony_omit_nested_state"`; it is not a Cargo feature.
A portable live-payload publication test fails under that configuration.

## Checks

Portable tests use scripted mock backends and cover the run loop, loaders,
protocol, virtual time, and snapshot/branch behavior. Live tests are selected
by platform and require the corresponding KVM or Hypervisor.framework host.

```sh
cargo test -p vmm-core
cargo clippy -p vmm-core --all-targets -- -D warnings
```

The SHA-256 qualification builds the production VMM and a software-backed
copy into one release executable. The copy uses the same VMM source and the
locked RustCrypto source archive, verified against its Cargo.lock checksum,
under separate package names so Cargo cannot unify the two hashing backends.
Only the control's hashing dependency changes. Neither copy is installed.

```sh
cargo fetch --locked
python3 consonance/vmm-core/qualification/qualify-sha256.py --check
python3 consonance/vmm-core/qualification/qualify-sha256.py
```

`--check` compares digests across padding boundaries, streaming chunk sizes,
unaligned inputs, cloned prefix states, and complete VMM state hashes. The
Snapshot and Restore and the macOS Arm64 and Linux Arm64 jobs of Harmony Host Compatibility run it without
timing thresholds. Backend selection is checked against the features reported
by the compiler artifacts for the actual build, including cached artifacts. The full run
also reports nine alternating software/native timing pairs for small and large
digests and whole-state hashes over zero and populated RAM from 4 KiB through
128 MiB. Each timed result is checked. ARM timings qualify this build change;
the software control deliberately disables acceleration on every architecture.
These are host hashing costs, not guest execution throughput. The executable's
SHA-256 is printed with the results to bind both arms to the same build.

The contract-cache qualification compares the production x86 or ARM fingerprint with a
same-source VMM copy whose only change bypasses the fingerprint cache. Both use
the same SHA backend in one executable. It checks concurrent first callers,
fresh-process initialization, zero allocations for warmed fingerprint reads,
complete state hashes and CPU/device captures, full-memory restores, and rejection
of a changed contract. ARM checks cover both ASID widths, including concurrent
initialization of the two caches and rejection of cross-width restores. Default
runs count allocations; use `--system-allocator` for timings with the normal
allocator and without allocation instrumentation. In that mode, the allocation
fields are disabled counters and must not be interpreted as allocation counts.
Capture timings cover CPU/device state, while restore timings include comparing
the complete RAM image. These mock-backend measurements do not measure guest
execution or hypervisor entry costs.

```sh
python3 consonance/vmm-core/qualification/qualify-contract.py --check
python3 consonance/vmm-core/qualification/qualify-contract.py --system-allocator
python3 consonance/vmm-core/qualification/qualify-contract.py --architecture arm64 --check
python3 consonance/vmm-core/qualification/qualify-contract.py --architecture arm64
python3 consonance/vmm-core/qualification/qualify-contract.py --architecture arm64 --system-allocator
python3 consonance/vmm-core/qualification/qualify-contract.py --miri
```

The full run reports nine alternating pairs at 4 KiB, 64 KiB, 1 MiB, and
128 MiB RAM. Allocation bytes are cumulative requests, not peak RSS. `--miri`
exercises the qualification allocator, including reallocation and deallocation.

The snapshot-capture qualification compares production control requests with a
same-source VMM copy that restores the two independent CPU captures. Both arms
run in one release executable, whose SHA-256 is printed with the results.

```sh
python3 consonance/vmm-core/qualification/qualify-capture.py --check
python3 consonance/vmm-core/qualification/qualify-capture.py
python3 consonance/vmm-core/qualification/qualify-capture.py --hvf --check
python3 consonance/vmm-core/qualification/qualify-capture.py --hvf
```

The portable check compares complete exported sidecars and full VMM state
hashes for x86 and ARM mock backends, including populated XSAVE and ARM SIMD
state. Snapshot receipts and cleanup are checked on every timed request. The
full run reports nine alternating timing pairs for snapshot-and-release over
16 KiB and 1 MiB RAM, with zero, sparse (every sixteenth page populated), and
dense contents. It also measures standalone public save-and-hash-encode calls.
Setup, VM creation, and export are outside the timed region; RAM hashing and
snapshot-store work are inside. These are snapshot costs, not guest execution
throughput. `--hvf` requires a real Apple silicon host with Hypervisor.framework;
it creates only one VM at a time. Both modes compare the canonical hashes
returned by portable snapshot export, which hashes the stored RAM and the
actual snapshot suffix, as well as standalone VMM hashes. Live HVF
stored sidecars may differ because the hardware virtual counter advances
between captures. Portable checks run in Snapshot and Restore and the macOS Arm64 and Linux Arm64 jobs of Harmony Host Compatibility without timing thresholds. Hosted macOS runners
cannot run the live HVF qualification because nested HVF is unavailable.

The SDK-capture qualification compares request-local SDK capture reuse against
capturing the environment again for the hash suffix, in one executable:

```sh
python3 consonance/vmm-core/qualification/qualify-sdk-capture.py --check
python3 consonance/vmm-core/qualification/qualify-sdk-capture.py
python3 consonance/vmm-core/qualification/qualify-sdk-capture.py --hvf --check
python3 consonance/vmm-core/qualification/qualify-sdk-capture.py --hvf
```

It compares standalone and exported hashes and complete mock sidecars. Fixtures
cover the default nominal SDK, empty custom handlers, handler states of 4 KiB, 64 KiB, and 1 MiB,
and 256 recorded answers plus 256 pending payloads of 256 bytes each. The
16 KiB guest RAM stays zero-filled. The measured operation is snapshot-and-drop;
setup, VM creation, export, and independent hash reads are outside the timing.
Nine alternating pairs report host snapshot cost, not guest throughput. Live
HVF uses one VM at a time and compares canonical hashes; advancing hardware
counters prevent raw sidecar comparison. CI runs portable checks without timing
thresholds in Snapshot and Restore and the macOS Arm64 and Linux Arm64 jobs of Harmony Host Compatibility.

The SDK capture driver also accepts `--encoding` to compare direct encoding into
the canonical hash suffix with the original intermediate-buffer encoder. Both
arms reuse the SDK capture; the only difference is buffer construction. It uses
the same fixtures, hash/sidecar checks, timing boundaries, and optional `--hvf`
mode. Portable `--encoding --check` runs in the same CI jobs. The checked-in
reference encoder also checks all pending-stop variants, pending-snapshot flags,
and empty/populated coverage thresholds against direct encoding in unit tests.
Both comparisons use the host system allocator. Each run prints its allocator, comparison,
and executable identity. The two arms always run inside the same executable.

The control-state capture qualification compares reuse of serialized control
history against serializing it again for storage, in one executable:

```sh
python3 consonance/vmm-core/qualification/qualify-control-capture.py --check
python3 consonance/vmm-core/qualification/qualify-control-capture.py
python3 consonance/vmm-core/qualification/qualify-control-capture.py --hvf --check
python3 consonance/vmm-core/qualification/qualify-control-capture.py --hvf
```

It uses the same snapshot-and-drop timing and hash/sidecar checks as the SDK
capture qualification, with 16 KiB zero RAM. Fixtures exercise an empty plan,
4 KiB, 64 KiB, and 512 KiB of payloads, 64 KiB of recorded answers, and 64 KiB
of pending memory-write effects. Data is split into 256-byte entries. The
pending effects are never executed in the timed region. Portable checks run
in the existing snapshot and ARM host CI jobs without timing thresholds.

Add `--inputs` to compare direct nested input encoding with the prior buffered
encoder, leaving control-state encoding reuse enabled in both arms. The input
reference uses public accessors and the standalone configuration/effect encoders
to reproduce the old framing. Its bytes are also checked by environment unit
tests over all effect variants and tape shapes. The probe uses the host system
allocator, matching the macOS CLI. Each run identifies
the allocator and comparison. CI runs `--inputs --check` without timing limits.

Add `--borrowed` instead of `--inputs` to compare borrowing the recorded history
with cloning it for capture. Both arms keep the direct nested encoder and
control-state serialization reuse. Whole-control hash requests, exported
snapshot hashes, and VMM hashes must agree; mock runs also compare all sidecar
bytes. The `reference` and `optimized` row labels always identify the two arms.
CI runs `--borrowed --check` without timing limits.

The SDK-restore qualification compares this path with a same-source VMM copy
that clones the entire SDK snapshot and restores the replay environment twice.
Both versions run in one release executable, identified by its printed SHA-256.

```sh
python3 consonance/vmm-core/qualification/qualify-restore.py --check
python3 consonance/vmm-core/qualification/qualify-restore.py
```

It checks repeated Replay and Branch requests on mock x86 and ARM machines in
both memcpy and in-place restore modes. Each arm captures the resulting VM and
compares complete sparse sidecars across versions; replay also reproduces its
original sidecar, and neither path changes the reusable source snapshot. The
fixtures have 16 KiB RAM and service-handler states of 0, 4 KiB, 64 KiB, and
1 MiB. A separate history fixture contains 256 recorded answers and 256 pending
payloads of 256 bytes each. The full run reports nine alternating timing pairs. `--filter handler_4096`
repeats just the small-state cases. Timing includes the complete control restore operation; construction, source
snapshot capture, and output comparisons are outside the measured loop. These
measure host restore work above mock backends, not live hypervisor or guest
throughput. Snapshot and Restore and the macOS Arm64 and Linux Arm64 jobs of Harmony Host Compatibility run
`--check` without timing thresholds.

The x86 exit dispatcher finishes the current instruction's device-access chain
before returning a stopped endpoint. Continuation accesses retain their device,
virtual-time, and trace accounting, but do not enter the next guest instruction
or deliver new scheduled inputs between fragments. Snapshot capture performs no
completion work. A periodic trace checkpoint crossed inside an instruction
lands on its final access; deferred hash consumers use
`virtual_time_checkpoint_due` to identify that exact capture position.

`Vmm::arm_checkpoint_hash_preimage` enables one bounded retained record for
the most recent synchronous checkpoint. The record returned by
`take_checkpoint_hash_preimage` contains the completed trace event index, the
published state hash, exact state-blob suffix, RAM length, and the SHA256 digest
of the `MEM\0 || length_le64 || RAM` prefix. The memory digest reuses a cloned
hash context before appending the suffix; it adds no RAM traversal. It
reuses the checkpoint's existing backend save and does not copy guest RAM or
perform another CPU read; calling the arm method again clears the prior
record. The feature is disabled by default and captures only completed
checkpoint boundaries, so it is suitable for a paired diagnostic that already
retains the corresponding RAM image. Read the record and RAM immediately after
that `step` returns, before advancing or otherwise mutating the VM; the record
alone does not freeze RAM. Hashing retains its original RAM-then-CPU-read order.

`Checks / Consonance` combines the portable contract suite with bounded
hardware checks on every pull request, and `Checks / Consonance / Analysis`
carries the Miri, coverage, mutation and proof work. The KVM job requires the
published snapshot identity/replay matrix and serviced-exit checks. The platform
job compares two complete same-seed Linux execution logs, requiring guest
readiness, nonzero events and zero differences. The scheduled and dispatched
`Checks / Consonance / Hardware Qualification` retains broader serviced-exit,
RF, PAE translation and guest-written XSAVE coverage, including dirty reused
vCPUs, and `Checks / Harmony Workloads / OCI` retains the full-state workload
restore oracle.

Linux snapshot fixtures come from the shared source-keyed platform
publisher. The bounded check requires exact source provenance, verifies the manifest,
and uses its direct Linux fixture. The scheduled/manual producer builds the
Nix kernel once, packages the runtime fixture without another kernel build,
and publishes only after platform replay passes. Execution stays bounded
independently of builds, and failed checks retain diagnostics. Broader repetitions and vendor sampling
remain scheduled/manual. These checks are regression evidence, not a claim that
all XSAVE-presence and AMD NPT PAE behavior is resolved.

`ControlServer::export_sparse_delta(setup, parent, target)` returns a
snapshot's pages over its parent, with their hashes, the pages that returned to
the setup snapshot's content, and the sparse sidecar. The pages borrow the
store. `import_sparse_delta(setup, near, pages, sidecar)` takes a complete
page list over the setup snapshot and derives the imported snapshot from `near`,
writing only the pages where the two differ. Once `near`'s chain reaches the
maximum chain length, the import derives from `setup` instead, so repeated
imports keep the same chain bound as seals. Each page must match its hash, so a
changed page is refused before a handle is minted.

Full and sparse portable imports share the VMM's read-only restore preparation
before entering the snapshot store. Invalid engine state, XSAVE provenance,
device records, and clock wiring are rejected before changing the destination
execution. Import requires a live validation target. This preflight does not
replace backend validation or make host ioctl failures transactional.

## Publishing prepared snapshot boundaries

The x86 Linux composition prepares its initial CPU state before publishing the
VM. The VMM prepares each fully serviced exit before checkpoint hashing, and
full-memory and control-server restores prepare after installing RAM and CPU
state. `Vmm::prepare_snapshot` is explicit for low-level callers that construct
or restore CPU state separately from memory; call it only once the complete
boundary is installed. Snapshot reads and hash reads do not enter KVM.
Entering the backend invalidates snapshot publication until exit servicing and
preparation succeed. A failed entry, completion chain, preparation, or live
restore cannot publish a cached CPU image as a new snapshot or hash. Raw backend
register reads remain available to service an exit; they are not snapshot
admission checks.

A control-server snapshot captures the raw CPU state once and uses that same
request-local capture for both the stored VM state and the canonical hash
suffix. Snapshot admission checks still run before publication, and the SDK
snapshot stays between VM-state construction and suffix construction. No guest
execution or backend mutation occurs between these consumers. The raw capture
is discarded after the request; it is not cached for later snapshots. Stored
ARM timer counters and canonical counter normalization retain their distinct
representations. Standalone snapshot and hash reads retain their own capture
and readiness checks.

The same control snapshot also reuses its SDK environment capture when encoding
the canonical hash suffix. The saved handler state, recorded answers, and
pending payloads are captured once per request, after snapshot admission and
before publication. No execution or SDK mutation occurs before suffix encoding.
Standalone hash reads capture their own SDK state; nothing is cached across
requests. SDK capture errors still poison the server before a snapshot is
published. The wire encoding and hashed bytes are unchanged. The SDK record is
encoded directly into the hash suffix; its two nested length fields are filled
from the resulting byte ranges. This avoids temporary record and chunk buffers
while preserving the same framing and independently owned retained SDK state.

A control snapshot encodes its recorded inputs, pending effects/reseeds, failure
state, and execution nonce once. It appends these encoded bytes to the canonical
hash suffix and moves that same encoding into retained snapshot metadata. This
avoids a second serialization of the history and plan; each request still
captures fresh state and retains the same independently owned metadata.
Control capture borrows the recorded input history only until serialization
finishes, avoiding a temporary deep copy of its payloads and answers. Pending
plans are still assembled independently. Decoding always produces owned inputs,
and retained snapshots never borrow the live controller.

KVM preparation round-trips FPU state without executing a guest instruction,
while preserving modeled RAM, CPU fields other than hardware XSAVE presence,
and execution accounting. Raw XSAVE presence remains restoration metadata; the
logical projection below decides how it enters identity. The execution
requirement is one qualified host core type
for related boots and restores; see the backend README for affinity admission and
its limits. Cross-type migration is not supported. Cross-host placement and broader
XSAVE state lie outside the qualified set.

The supported x86 workload is the shipped 64-bit Linux kernel and initramfs on
a 64-bit host. Application bitness is separate: 32-bit compatibility-mode
userspace can run under long-mode paging. [AMD's architecture manual](https://www.amd.com/content/dam/amd/en/documents/processor-tech-docs/programmer-references/24593.pdf)
requires CR4.PAE in long mode, so the bit does not, by itself, select legacy
32-bit PAE paging. Linux-composed VMs reject
CPU records without active long-mode paging (CR0.PG, CR4.PAE, EFER.LMA) when
publishing snapshots and before restoring snapshots. State hashes can still
observe transient CPU modes during Linux boot; a hash is not a snapshot
admission decision. Generic
VMM instances remain available for synthetic CPU-mode diagnostics. The backend
does not expose every guest mode transition as a checked boundary, so these
admission checks do not confine
an arbitrary kernel that changes modes between observed boundaries.

The shipped Linux guest follows architectural page-table update and invalidation
rules and cannot replace its kernel through kexec. Required PAE continuation
coverage reloads CR3 after a guest-authored PDPT update, comparing reference,
captured, cold-restored, and reused-restored endpoints. Intel's cached-PDPTR
preservation regression remains required. AMD NPT fixtures that rely on stale
PDPTR persistence without invalidation remain recorded informational diagnostics;
they do not define the supported Linux guest contract. The loader's 64-bit entry
check does not establish the later mode behavior of arbitrary supplied kernels.

The required MMIO full-snapshot fixture creates active XMM0 data with a guest
PCMPEQD before the LAPIC read/modify/write. It checks the captured value and a
subsequent guest MOVDQU store, while retaining exact RAM, CPU, serialized state
and hash comparisons and the MMIO completion/timing assertions. There is no
extra guest exit, warmup or host restore-bitmap forcing. The original init-only
program remains an informational characterization using the same exercise.

### Published XSAVE identity check

`vendor::x86::logical_identity_live_tests::public_snapshot_replay_recapture_preserves_xsave_identity`
exercises
`ControlServer`'s published Snapshot, Replay and portable export APIs. It mints a
new snapshot after restore, rather than re-exporting the original handle. The
required fixed-core CI check covers raw init seeds 0/2/3, XCR0 3/7, initial and
serviced UART boundaries, init and active register values, fresh and verified
in-place restores, repeated captures, and three additional host-only preparation
entries. The guest dirties FP/vector/MXCSR state before replay. Independent
single-component changes to x87, XMM, YMM and MXCSR must change published identity
with identical guest RAM and program bytes.

The check compares logical hashes and complete persisted execution state, then
resumes both paths to the same guest endpoint. Comparisons validate each artifact
before projecting only the permitted raw restoration metadata. RAM, canonical
CPU state, MXCSR, devices and control state remain part of the comparison. The
existing diagnostic trace counters remain excluded across replay. Artifact
checksums still cover every original byte, including raw restoration metadata.

On fixed-core AMD, an extra host-only preparation entry can change the published
hash with no guest instruction executed. The raw restore bitmap changes from 0
to 2 while RAM and every other serialized CPU field stay identical. The ordinary
two-boot Linux check reproduces the same field difference: matching event context
and RAM digest, and exactly one changed byte in VCPU's XSRB field. RAM is
compared there by digest rather than bytewise. Intel's fixed-P-core matrix
passes. Avoiding duplicate preparation calls does not remove the difference, and
a passing retry does not establish raw identity stability across extra entries.

Logical identity separates logical state from raw restoration metadata.
`logical_xsave_restore_bv` validates the standard XSAVE image and its raw
provenance, then removes only raw x87/SSE presence bits for components already
absent from the canonical image. The condition is decided from the snapshot
bytes alone, so it needs no list of approved guests. Every other presence bit,
canonical component byte, MXCSR and all non-CPU state remain significant. Both
VCPU and VMST hashing use the same projection through
`Vendor::logical_identity_vcpu`. `save_vm_state` and portable exports retain the
complete original metadata for restoration: equal logical identities can
therefore have different artifact bytes and artifact checksums.

Equal logical identity is not a proof that the guests are interchangeable. It
assumes the admitted kernel and userspace execution contract: no unreviewed
executable code, code mutation, raw userspace XSAVE-family saves, or XGETBV with
ECX=1. It does not qualify arbitrary ROMs, SQL, injected machine state or
external events merely because the initial image matches.

The published API regression remains required on AMD and Intel under supported
core placement. A successful regression does not establish raw bitmap stability
or support for arbitrary guest code.

An SDK coverage exchange checks the thread's expected threshold, asks the
scheduler service which thread runs, and asks environment service 4 for the
next quantum. The next threshold is part of the SDK snapshot. A thread whose
count restarts at the first threshold gets a fresh entry, because Linux reuses
thread IDs. A zero, malformed or overflowing quantum leaves the environment and
every threshold unchanged.
