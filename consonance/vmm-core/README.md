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

Guest RAM is owned by `Vmm` for the lifetime of the backend. The state hash and
snapshot machinery include all observable guest memory, vCPU state, device
state, timer state, virtual time, entropy, control state, and protocol state.
Snapshots can be restored into a copy-on-write memory mapping. SDK state
capture retains pending stops and unanswered service requests without consuming
them, including the response sequence and request identity. Portable format 4
carries this state; version 3 remains readable without pending stops. Portable
format 5 adds pending host effects and reseeds, the recorded input prefix,
schedule failure, and command nonce. Replay restores these without reseeding or
reapplying consumed inputs; an explicit branch selects a new plan and retains
the command nonce. A retained command uses control-state record `HCSTATE2` to
carry its serial output cursor, bounded completion parser, output, and actual
completion status/moment. Both replay and branch retain this physical command;
restoring never reinjects its input. Records without a command retain `HCSTATE1`
bytes. `ExecStart` injects at the current stop without running; `ExecStatus` is
pure observation. A deadline leaves a command pending, while a terminal guest
without a completion sentinel records an aborted command with unknown status.
Unread serial input belongs to the UART snapshot. Nonempty receive queues use
`DEV2` (x86) or `ADV2` (ARM) device records; empty queues retain their existing
record bytes. Restore replaces the queue with its remaining bytes. Pending
command parser bytes are checked against the restored serial output before
execution can resume; inconsistent state abandons the restored VM.
Whole-state hashes include the UART queue and control state, including the
recorded prefix used for duplicate-input rejection. Legacy v3/v4 artifacts remain
readable with their historical empty control-state default; their recorded hash
uses the old coverage. Representable states retain their v4/v5 bytes. Portable
format 6 carries sections beyond the older fixed limits, including large unread
UART input and SDK streams. It keeps the v5 field order and allows an empty
control section. Complete imports allocate incrementally from bytes actually
read; sparse imports bound lengths by the available input. Declared lengths
alone cannot trigger an unbounded allocation. Legacy readers retain their
original limits, while unsupported versions fail explicitly.
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
and must survive restore without reloading them from that memory.

The live cached-PDPTR witness covers Intel paging and AMD shadow paging. A
separate `amd_default_npt_pae_snapshot_identity` gate exercises AMD with NPT
left enabled: it compares complete original, save-and-continue, and cold
endpoints after a guest page-table RAM change. Its uninterrupted arm does not
inspect CPU state at the stop. The x86 workflow's manual `amd_npt_snapshot`
input runs this focused gate and records the host mode without changing it.
The separate manual `amd_npt_observe` mode retains all paging-arm observations
before asserting endpoint identity; it provides diagnostic evidence when a
fixed paging expectation prevents the qualification test from reaching capture.
That mode also runs a direct KVM ioctl diagnostic, even if the identity test
fails, to compare individual register reads on fresh VMs pinned to one allowed
CPU. Its retained observations isolate capture effects; they are not a passing
snapshot qualification. A further observation has the guest change its own
PDPT before the stop, ruling out fixes that affect only host memory writes.
The observation workflow also compares a raw guest-written XSAVE area across
original, saved, and cold execution on either x86 vendor.

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
