<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# arm64 vendor

The arm64 vendor adapts the shared VMM engine to an arm64 `Image` boot. It
defines the board map, arm64 CPU policy, Image loader, entry state, DTB,
normalized exit dispatch, device wiring, host checks, and snapshot record
conversions.

The fixed board places RAM at `0x4000_0000` and the GIC distributor,
redistributor, PL011, doorbell, and pvclock/clockevent frames below it. The DTB
describes the same map. `bringup::compose` loads the image and optional
initramfs, reserves the pvclock page, builds the DTB, maps RAM and control pages,
and restores the entry state before returning a `Vmm`.

`dispatch` routes GIC, PL011, doorbell, and pvclock MMIO to the modeled devices.
The userspace `gicv3` model is used by the HVF composition; stock arm64 KVM
owns its GIC in the kernel and does not expose an arbitrary userspace INTID
injection fabric. The shared run loop therefore treats that distinction as a
backend boundary.

`records` converts live arm64 vCPU and GIC state to the `vm-state` records. The
architectural comparator in the parent module compares vCPU and canonical GIC
fields independently of snapshot hashes.
