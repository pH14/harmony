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
carries this state; version 3 remains readable with no pending stop. Whole-VM
capture still has backend and SDK-stop guards; it is side-effect-free for a
pending pvclock registration, carrying its GPA, `armed = false` state, and page
bytes so the next handshake resumes from the same state.
Pvclock-bearing device records explicitly preserve the registered page GPA,
registration capability, and pending-versus-armed handshake state. Pending
registrations use x86 v5 and arm64 v9–12; already-representable states retain
legacy x86 v4 and arm64 v5–8 bytes, where a GPA implies an armed registration.

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
