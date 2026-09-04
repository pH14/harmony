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
Snapshots are taken at quiescent boundaries and can be restored into a
copy-on-write memory mapping.

## Architecture boundary

The engine uses only common exits, guest-physical addresses, bytes, and typed
vendor traits. `vendor/x86` supplies the x86 CPU policy, loaders, device
dispatch, and records. `vendor/arm64` supplies the arm64 Image/DTB boot path,
board devices, policy, and records. The arm64 vendor is also used to exercise
the additive architecture seam on portable mocks and QEMU.

## Checks

Portable tests use scripted mock backends and cover the run loop, loaders,
protocol, virtual time, and snapshot/branch behavior. Live tests are selected
by platform and require the corresponding KVM or Hypervisor.framework host.

```sh
cargo test -p vmm-core
cargo clippy -p vmm-core --all-targets -- -D warnings
```
