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
VMST switch is still default-off for legacy paths; a present v6
`xsave_restore_bv` intentionally changes the VCPU identity in either mode.
Snapshots can be restored into a copy-on-write memory mapping. SDK state
capture retains pending stops and unanswered service requests without consuming
them, including the response sequence and request identity. Portable format 4
carries this state; version 3 remains readable without pending stops. Portable
format 5 adds pending host effects and reseeds, the recorded input prefix,
schedule failure, and command nonce. Replay restores these without reseeding or
reapplying consumed inputs; an explicit branch selects a new plan and retains
the command nonce. Whole-state hashes include this control state, including the
recorded prefix used for duplicate-input rejection. Legacy v3/v4 artifacts remain
readable with their historical empty control-state default; their recorded hash
uses the old coverage. The codec retains v4 bytes when control state is absent.
Portable format 6 carries sections that exceed the legacy envelope limits,
including long SDK event histories; smaller records retain their v4/v5 bytes.
Complete reads allocate incrementally from received bytes, and sparse section
lengths are bounded by the supplied input. Readers retain v3–v5 compatibility.
Whole-VM capture preserves pending SDK stops and is side-effect-free for a
pending pvclock registration, carrying its GPA, `armed = false` state, and page
bytes so the next handshake resumes from the same state.
Pvclock-bearing device records explicitly preserve the registered page GPA,
registration capability, and pending-versus-armed handshake state. Pending
registrations use x86 v5 and arm64 v9–12; already-representable states retain
legacy x86 v4 and arm64 v5–8 bytes, where a GPA implies an armed registration.

The architecture-neutral engine record preserves terminal reasons and deferred
SDK reentry state. Nonempty records use VM-state container v4; ordinary runnable
states retain v3 bytes. Legacy v3 records remain readable with the historical
runnable lifecycle default. A terminal restore does not enter the guest again.

X86 CPU capture retains SREGS2 flags and cached PAE PDPTRs, plus debug-register
flags. Nonzero extended fields select VM-state v5; zero values retain v3/v4
bytes. Cached PDPTRs are distinct from the current PDPT contents in guest RAM
and must survive restore without reloading them from that memory. Every
standard-format XSAVE capture retains the original `XSTATE_BV` in the v6
tag-15 record, whether or not canonicalization changes the x87/SSE init-state
bits. The value is validated before restore and included in both the vCPU
identity and the complete VMST identity when snapshot hashing is wired. Short
or compacted images retain the legacy no-provenance behavior. Always retaining
the field for standard captures also avoids a 14-byte record-length variation
crossing a sparse sidecar charge boundary. A matching fingerprint is not a proof
of whole-guest future equivalence; focused guest-byte coverage remains required.
Legacy records without that field keep their historical normalized restore
behavior and cannot recover discarded header provenance.

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

PR acceptance combines the portable contract suite and selected Miri checks
with bounded hardware gates. `x86-virtual-time.yml` checks serviced exits, RF,
PAE translations, and guest-written XSAVE output, including a reused VM whose
FPU state was changed by another continuation. `snapshot-linux-smoke.yml`
compares two complete same-seed Linux execution logs; its success requires
nonzero events and zero differences. Workload acceptance smokes exercise
continuations through their restore oracles.

Linux smoke fixtures come from main's durable guest cache. Each run verifies
the manifest and records the exact or last-known-good cache provenance. A
fallback fixture does not validate changed guest source; the scheduled/manual
builder supplies that evidence. Execution is bounded independently of builds,
and failed gates retain diagnostics. Broader repetitions and vendor sampling
remain scheduled/manual. These gates are regression evidence, not a claim that
the retained XSAVE-presence and AMD NPT PAE counterexamples are resolved.

Full and sparse portable imports share the VMM's read-only restore preparation
before entering the snapshot store. Invalid engine state, XSAVE provenance,
device records, and clock wiring are rejected before changing the destination
execution. Import requires a live validation target. This preflight does not
replace backend validation or make host ioctl failures transactional.
