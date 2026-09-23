<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# vmm-core

`vmm-core` is the deterministic VMM above the `vmm-backend::Backend` trait. It
owns the run loop, guest RAM, virtual-time advancement, entropy, device
dispatch, hypercall/control handling, snapshot and branch operations, and
state hashing. Host hypervisor calls stay behind the backend trait; concrete
backend and architecture pairs are selected by the vendor composition roots.

## Run loop

`Vmm::run` repeatedly obtains one backend exit, classifies it through the
architecture vendor, advances virtual time by the assigned integer duration,
dispatches devices and protocol services, and completes any pending backend
operation. Timer deadlines are applied at exit boundaries. An idle guest can
advance to the next deterministic deadline through the same clock; no host
clock is consulted.

Guest RAM is owned by `Vmm` for the lifetime of the backend. The canonical state
fingerprint and snapshot machinery cover guest memory, vCPU state, device state,
timer state, virtual time, entropy, control state, and protocol state. Vendor
fingerprint encodings cover the complete state records used for restore, while
the complete portable artifact digest covers the same persisted bytes. The
VMST uses the current version 6 wire format; a present `xsave_restore_bv`
intentionally changes the VCPU identity.
Snapshots can be restored into a copy-on-write memory mapping. Portable format
6 preserves pending SDK stops, unanswered service requests, response sequences,
pending host effects and reseeds, the recorded input prefix, schedule failure,
and command nonce. Replay restores these without reseeding or reapplying consumed
inputs; an explicit branch selects a new plan and retains the command nonce.
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
flags, in the current VM-state v6 records. Cached PDPTRs are distinct from the
current PDPT contents in guest RAM and must survive restore without reloading
them from that memory. Every standard-format XSAVE capture retains the original
`XSTATE_BV` in the v6 tag-15 record, whether or not canonicalization changes the
x87/SSE init-state bits. The value is validated before restore. It is included in
both the vCPU identity and the complete VMST identity through the logical
projection described below. Short or compacted images may omit that optional
provenance field. A
matching fingerprint is not a proof of whole-guest future equivalence; focused
guest-byte coverage remains required.

Hardware continuation coverage depends on the backend and paging mode. AMD
default NPT has an unresolved PAE capture divergence
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
Each architecture supplies one guest machine policy; the backend supplies the
required virtualization capabilities. The x86 runtime boots controlled Linux on stock KVM; instruction interception
patches, Multiboot payloads, and the legacy acceptance runner have been retired.
The x86 policy and snapshot compatibility
rules are documented in [contracts/x86](contracts/x86/README.md).

## Checks

Portable tests use scripted mock backends and cover the run loop, loaders,
protocol, virtual time, and snapshot/branch behavior. Live tests are selected
by platform and require the corresponding KVM or Hypervisor.framework host.

```sh
cargo test -p vmm-core
cargo clippy -p vmm-core --all-targets -- -D warnings
```

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

KVM preparation round-trips FPU state without executing a guest instruction,
while preserving modeled RAM, CPU fields other than hardware XSAVE presence,
and execution accounting. Raw XSAVE presence remains restoration metadata; the
logical projection below decides how it enters identity. The execution
requirement is one qualified host core type
for related boots and restores; see the backend README for affinity admission and
its limits. Cross-type migration is not supported. Cross-host placement and broader
XSAVE state lie outside the qualified set.

The shipped Linux guest follows architectural page-table update and invalidation
rules and cannot replace its kernel through kexec. Required PAE continuation
coverage reloads CR3 after a guest-authored PDPT update, comparing reference,
captured, cold-restored, and reused-restored endpoints. Intel's cached-PDPTR
preservation regression remains required. AMD NPT fixtures that rely on stale
PDPTR persistence without invalidation remain recorded informational diagnostics;
they do not define the supported Linux guest contract. Arbitrary supplied kernels
are not confined to that contract by their initial long-mode entry.

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
