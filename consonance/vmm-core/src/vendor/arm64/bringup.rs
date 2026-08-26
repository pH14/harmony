// SPDX-License-Identifier: AGPL-3.0-or-later
//! The arm64 boot composition (`tasks/112` M3) — the arm64 analogue of x86's
//! `bringup::compose`: install the CPU-contract policy **through the trait**,
//! allocate RAM, flat-load the `Image`, build + place the DTB, build + restore
//! the entry state, map the RAM, and return a [`Vmm`] ready to `run()`.
//!
//! [`compose`] takes the `Backend` **by value** (constructed bare at the
//! composition root; policy goes in through [`Backend::set_policy`], not a
//! concrete constructor), so the composition — including the `unsafe`
//! `map_memory` pointer seam — is unit-testable with the `MockArm64Backend` on
//! every platform (and under Miri). The one place a concrete
//! `(Arm64KvmBackend, Arm64)` pair is named is the M4 `boot_selected`
//! (Linux+aarch64-gated) — not here.
//!
//! **The interrupt fabric is left unwired** (`docs/ARCH-BOUNDARY.md` §D / M2
//! §Delivery): the stock `Arm64KvmBackend`'s `set_pending_irq` is `Unsupported`
//! and guest delivery is AA-6-gated (the vGICv3 round-trip verdict), so a
//! stock-safe boot root never wires the userspace GICv3. The DTB still
//! advertises the GICv3 so a guest can program it; wiring its delivery is a
//! later bead.

use vmm_backend::{Arm64, Backend, Gpa};

use super::board::{PAGE, RAM_BASE, align_up};
use super::{contract, dtb, entry, hostassert, image_loader};
use crate::vmm::{GuestRam, Vmm, VmmError};

/// Boot an arm64 `Image`: the host-baseline gate
/// ([`hostassert::enforce`](super::hostassert::enforce)) **then** [`compose`].
/// Takes the `Backend` by value (constructed bare at the composition root),
/// mirroring x86's `boot`. The one place a concrete `(Arm64KvmBackend, Arm64)`
/// pair is named is the M4 `boot_selected` (Linux+aarch64-gated).
pub fn boot<B: Backend<A = Arm64>>(
    backend: B,
    image: &[u8],
    bootargs: &str,
    guest_ram_len: usize,
) -> Result<Vmm<B>, VmmError> {
    hostassert::enforce()?;
    compose(backend, image, bootargs, guest_ram_len)
}

/// Compose a ready [`Vmm`] for an arm64 `Image` boot, **without** the
/// host-baseline gate (so the composition — including the `unsafe` `map_memory`
/// seam — is unit-testable with a mock backend on every platform). Order is
/// load-bearing:
/// policy **before** the first run; map **before** restore; `ram` moves into
/// the `Vmm` so the mapped pointer stays valid.
///
/// # Errors
/// [`VmmError::vendor_boot`] wrapping an [`image_loader::ImageLoadError`] (a
/// malformed image or one that does not fit alongside the DTB), or a
/// [`VmmError::Backend`] from policy install / map / restore.
pub(crate) fn compose<B: Backend<A = Arm64>>(
    backend: B,
    image: &[u8],
    bootargs: &str,
    guest_ram_len: usize,
) -> Result<Vmm<B>, VmmError> {
    compose_inner(backend, image, None, bootargs, guest_ram_len, true)
}

/// Shared arm64 composition with an explicit control-channel mapping choice.
/// HVF maps guest memory in 16-KiB host-page units, while the legacy doorbell
/// transport is an 8-KiB low-GPA region. The M1 Linux boot has no control
/// channel, so its HVF root deliberately omits that incompatible mapping.
fn compose_inner<B: Backend<A = Arm64>>(
    mut backend: B,
    image: &[u8],
    initramfs: Option<&[u8]>,
    bootargs: &str,
    guest_ram_len: usize,
    map_doorbell: bool,
) -> Result<Vmm<B>, VmmError> {
    // 1. Install the contract policy skeleton through the trait, before the
    //    first run (the arm64 `ID_AA64*` freeze + trapped-sysreg table; rows
    //    TODO(AA-6)).
    backend.set_policy(&contract::policy())?;

    // 2. Allocate RAM and flat-load the Image.
    let mut ram = GuestRam::new(guest_ram_len)?;
    let loaded = image_loader::load(image, ram.as_mut_bytes()).map_err(VmmError::vendor_boot)?;

    // 3. Lay out RAM above the loaded image, page-aligned: an optional external
    //    initramfs, the **reserved pvclock page** (the hm-rk5 seam), then the
    //    DTB. Placing
    //    pvclock before the DTB makes its GPA depend only on the kernel extent —
    //    not the DTB length — so the DTB (whose `/reserved-memory` child's
    //    node name is `pvclock@<hex(gpa)>`, a variable-length unit-address) is
    //    built **once**, with no circular size↔name dependency.
    let ram_len = u64::try_from(guest_ram_len)
        .map_err(|_| VmmError::ContractViolation("arm64 guest RAM length exceeds u64".into()))?;
    let (initrd_layout, post_initrd_off) = if let Some(bytes) = initramfs {
        let start_off = align_up(loaded.end_off, PAGE);
        let byte_len = u64::try_from(bytes.len()).map_err(|_| {
            VmmError::ContractViolation("arm64 initramfs length exceeds u64".into())
        })?;
        let end_off = start_off.checked_add(byte_len).ok_or_else(|| {
            VmmError::ContractViolation("arm64 initramfs extent wraps address space".into())
        })?;
        let start_gpa = RAM_BASE.checked_add(start_off).ok_or_else(|| {
            VmmError::ContractViolation("arm64 initramfs start GPA wraps address space".into())
        })?;
        let end_gpa = RAM_BASE.checked_add(end_off).ok_or_else(|| {
            VmmError::ContractViolation("arm64 initramfs end GPA wraps address space".into())
        })?;
        (Some((start_off, end_off, start_gpa, end_gpa)), end_off)
    } else {
        (None, loaded.end_off)
    };
    let pvclock_off = align_up(post_initrd_off, PAGE);
    let pvclock_gpa = RAM_BASE.checked_add(pvclock_off).ok_or_else(|| {
        VmmError::ContractViolation("arm64 pvclock GPA wraps address space".into())
    })?;
    let pvclock_end = pvclock_off.checked_add(PAGE).ok_or_else(|| {
        VmmError::ContractViolation("arm64 pvclock extent wraps address space".into())
    })?;
    let dtb_off = align_up(pvclock_end, PAGE);
    let dtb_gpa = RAM_BASE
        .checked_add(dtb_off)
        .ok_or_else(|| VmmError::ContractViolation("arm64 DTB GPA wraps address space".into()))?;
    let dtb_bytes = if let Some((_, _, start_gpa, end_gpa)) = initrd_layout {
        dtb::build_with_initrd(ram_len, pvclock_gpa, bootargs, start_gpa, end_gpa)
    } else {
        dtb::build(ram_len, pvclock_gpa, bootargs)
    };

    let dtb_start = usize::try_from(dtb_off)
        .map_err(|_| VmmError::ContractViolation("arm64 DTB offset exceeds host usize".into()))?;
    let dtb_end = dtb_start.checked_add(dtb_bytes.len()).ok_or_else(|| {
        VmmError::ContractViolation("arm64 DTB extent wraps host address space".into())
    })?;
    let ram_bytes = ram.as_mut_bytes();
    if dtb_end > ram_bytes.len()
        || pvclock_end > ram_len
        || initrd_layout.is_some_and(|(_, end, _, _)| end > ram_len)
    {
        return Err(VmmError::ContractViolation(format!(
            "arm64 boot: image + initramfs + DTB + reserved pvclock page do not fit in {guest_ram_len:#x} \
             bytes of guest RAM (DTB ends at {dtb_end:#x}, pvclock page at \
             {:#x})",
            pvclock_gpa - RAM_BASE
        )));
    }
    if let (Some(bytes), Some((start, end, _, _))) = (initramfs, initrd_layout) {
        let start = usize::try_from(start).map_err(|_| {
            VmmError::ContractViolation("arm64 initramfs offset exceeds host usize".into())
        })?;
        let end = usize::try_from(end).map_err(|_| {
            VmmError::ContractViolation("arm64 initramfs end exceeds host usize".into())
        })?;
        ram_bytes[start..end].copy_from_slice(bytes);
    }
    ram_bytes[dtb_start..dtb_end].copy_from_slice(&dtb_bytes);

    // 4. Map the RAM into the backend; it retains a pointer into `ram`.
    // SAFETY (granted purpose 2, mirroring x86 `compose`): `ram` is moved into
    // the returned `Vmm` in step 6 and its mmap/Vec backing does not move, so
    // the pointer stays valid for the backend's lifetime; the run loop holds
    // `&mut self`, so the backing is never aliased while a run is in flight;
    // GuestRam's off-Miri backing is a page-aligned mmap as
    // KVM_SET_USER_MEMORY_REGION requires. The guest RAM is mapped at RAM_BASE
    // (arm64 RAM is high; device frames sit below it, so no memslot split).
    unsafe {
        backend.map_memory(Gpa(RAM_BASE), ram.as_mut_bytes())?;
    }

    // 5. Build + restore the entry state, overlaid onto a live `save()`
    //    template (keeping the backend's valid EL1 sysreg shape — the arm64
    //    get→modify→set pattern).
    let entry_state = entry::boot_entry(loaded.entry_gpa, dtb_gpa);
    let mut state = backend.save()?;
    entry::apply_entry(&mut state, &entry_state);
    backend.restore(&state)?;

    // 6. Hand the configured backend + owned RAM to the Vmm, record the high RAM
    //    base, and map the hypercall-transport ABI pages as a dedicated low-GPA
    //    memslot. arm64 RAM is high (RAM_BASE), so the absolute ABI GPAs
    //    (REQ_GPA/RESP_GPA) fall below it and cannot be the main RAM's offset —
    //    tasks/112 keeps the transport magic unchanged, which favors mapping the
    //    absolute pages over per-arch GPA translation (see Vmm::map_doorbell_pages).
    let mut vmm = Vmm::new(backend, ram);
    vmm.ram_base_gpa = RAM_BASE;
    if map_doorbell {
        vmm.map_doorbell_pages()?;
    }
    Ok(vmm)
}

/// Compose the measured macOS/arm64 Hypervisor.framework backend for the M1
/// Linux boot. The userspace GICv3 is wired because HVF surfaces its CPU
/// interface sysregs and accepts pending IRQ injection at the vCPU boundary.
/// The legacy 8-KiB doorbell mapping is intentionally absent; M1 has no SDK
/// control channel and HVF requires 16-KiB guest mappings on this host.
///
/// # Errors
/// Returns the host-baseline, HVF construction, image, mapping, state, or GIC
/// composition error without falling back to a different execution path.
#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
pub fn boot_hvf(
    image: &[u8],
    initramfs: &[u8],
    bootargs: &str,
    guest_ram_len: usize,
) -> Result<Vmm<vmm_backend::HvfBackend>, VmmError> {
    hostassert::enforce()?;
    let backend = vmm_backend::HvfBackend::new()?;
    let mut vmm = compose_inner(
        backend,
        image,
        Some(initramfs),
        bootargs,
        guest_ram_len,
        false,
    )?;
    vmm.wire_gic(super::board::new_gic());
    vmm.wire_vtime(crate::vmm::VtimeWiring::new_prescriptive(
        vtime::VClockConfig {
            ratio_num: 1,
            ratio_den: 1,
            guest_hz: super::board::CNTFRQ_HZ,
            guest_base: 0,
            vns_base: 0,
        },
        0,
    )?);
    // Prescriptive mode stamps at exits and never uses the descriptive Δ
    // `run_until` path; the nonzero value remains part of snapshot identity.
    vmm.enable_pvclock(1);
    Ok(vmm)
}

/// **The composition root** (`tasks/112` M4): the one place the concrete
/// `(Arm64KvmBackend, Arm64)` pair is named — Linux+aarch64-gated, mirroring
/// x86's `boot_selected`. Constructs the stock KVM/arm64 backend
/// (`KVM_CREATE_VM` → `KVM_CREATE_VCPU` → `KVM_ARM_VCPU_INIT` in
/// `LiveKvm::new`), boxes it as `Box<dyn Backend<A = Arm64>>`, and [`boot`]s the
/// `Image`+DTB. No V-time is wired: the stock backend claims no determinism
/// (its `capabilities()` are honestly false), so the determinism path is a
/// later bead (the AA-3 patched backend + the paravirt clock, `hm-rk5`).
///
/// The real `KVM_RUN` boot to a console marker and the same-seed `state_hash`
/// determinism gate over this pair are **arrival-day**, edged to `hm-7pb` (the
/// Altra); there is no local KVM loop (`hm-8l3` REFUSE), so this root has no
/// local oracle — only the aarch64-linux cross-check compiles it.
///
/// **No interrupt-driven guest boot is claimed here** (`tasks/112` M2 §Delivery).
/// The stock backend wires **no** delivery fabric — `set_pending_irq`/inject are
/// `Unsupported`, and this root never creates an in-kernel
/// `KVM_DEV_TYPE_ARM_VGIC_V3`: guest interrupt delivery is `TODO(AA-6)` (the
/// vGICv3 round-trip verdict). So a guest that programs the GICv3 (the DTB
/// advertises it) and blocks on a device interrupt does **not** boot to
/// completion on this path — an interrupt-driven Linux is **deferred to AA-6**,
/// not offered by the skeleton. What this boots is the polled / PSCI-`SYSTEM_OFF`
/// console path (the M3 TCG smoke's shape).
///
/// # Errors
/// [`VmmError::Backend`] if `/dev/kvm` is unavailable or an init ioctl fails;
/// any [`boot`] error thereafter.
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub fn boot_selected(
    image: &[u8],
    bootargs: &str,
    guest_ram_len: usize,
) -> Result<Vmm<Box<dyn Backend<A = Arm64>>>, VmmError> {
    let live = vmm_backend::LiveKvm::new()?;
    let backend: Box<dyn Backend<A = Arm64>> = Box::new(vmm_backend::Arm64KvmBackend::new(live));
    boot(backend, image, bootargs, guest_ram_len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vmm_backend::MockArm64Backend;

    /// A tiny valid Image with a nonzero text_offset, so the load + DTB
    /// placement path is exercised end to end.
    fn tiny_image() -> Vec<u8> {
        // 256 bytes of "code" behind the header, page-aligned load.
        image_loader::wrap_image(&[0x42u8; 256], 0, 0xA)
    }

    #[test]
    fn compose_loads_image_places_dtb_and_sets_entry() {
        // 16 MiB RAM: room for the tiny image + DTB + reserved page.
        let ram_len = 16 * 1024 * 1024;
        let backend = MockArm64Backend::new();
        let vmm = compose(backend, &tiny_image(), "console=ttyAMA0", ram_len).unwrap();

        // The composed vCPU entered at RAM_BASE with x0 pointing at a DTB in RAM.
        let vcpu = vmm.inspect_vcpu();
        assert_eq!(vcpu.core.pc, RAM_BASE);
        assert_eq!(vcpu.core.pstate, entry::PSTATE_EL1H_DAIF);
        let dtb_gpa = vcpu.core.x[0];
        assert!(dtb_gpa > RAM_BASE && dtb_gpa < RAM_BASE + ram_len as u64);

        // The DTB actually landed at x0 and parses back.
        let off = (dtb_gpa - RAM_BASE) as usize;
        let mem = vmm.guest_memory();
        let parsed = dtb::parse(&mem[off..]).unwrap();
        assert!(parsed.nodes.iter().any(|n| n == "pl011@9000000"));
        // The reserved pvclock node's name is its `reg` address as unit-address
        // (`pvclock@<hex>`); its GPA is real, page-aligned RAM, and — with the
        // single-pass layout — sits below the DTB.
        let pvclock_node = parsed
            .nodes
            .iter()
            .find(|n| n.starts_with("pvclock@"))
            .expect("a pvclock reserved-memory node");
        let pv = parsed.prop(pvclock_node, "reg").unwrap();
        let pv_gpa = u64::from_be_bytes(pv[0..8].try_into().unwrap());
        assert_eq!(*pvclock_node, format!("pvclock@{pv_gpa:x}"));
        assert!(pv_gpa.is_multiple_of(PAGE));
        assert!(pv_gpa >= RAM_BASE && pv_gpa < dtb_gpa);
    }

    #[test]
    fn compose_linux_places_external_initramfs_and_describes_exact_range() {
        let ram_len = 16 * 1024 * 1024;
        let initramfs = vec![0xC3; 0x2345];
        let vmm = compose_inner(
            MockArm64Backend::new(),
            &tiny_image(),
            Some(&initramfs),
            "console=ttyAMA0",
            ram_len,
            true,
        )
        .unwrap();

        let dtb_gpa = vmm.inspect_vcpu().core.x[0];
        let dtb_off = usize::try_from(dtb_gpa - RAM_BASE).unwrap();
        let memory = vmm.guest_memory();
        let parsed = dtb::parse(&memory[dtb_off..]).unwrap();
        let start = u64::from_be_bytes(
            parsed.prop("chosen", "linux,initrd-start").unwrap()[..8]
                .try_into()
                .unwrap(),
        );
        let end = u64::from_be_bytes(
            parsed.prop("chosen", "linux,initrd-end").unwrap()[..8]
                .try_into()
                .unwrap(),
        );
        assert!(start.is_multiple_of(PAGE));
        assert_eq!(end - start, initramfs.len() as u64);
        assert!(end < dtb_gpa);
        let start_off = usize::try_from(start - RAM_BASE).unwrap();
        let end_off = usize::try_from(end - RAM_BASE).unwrap();
        assert_eq!(&memory[start_off..end_off], initramfs);
    }

    #[test]
    fn compose_linux_rejects_initramfs_that_does_not_fit() {
        let ram_len = 0x20_000;
        let initramfs = vec![0; ram_len];
        let result = compose_inner(
            MockArm64Backend::new(),
            &tiny_image(),
            Some(&initramfs),
            "",
            ram_len,
            true,
        );
        assert!(matches!(result, Err(VmmError::ContractViolation(_))));
    }

    #[test]
    fn compose_rejects_an_image_that_does_not_fit() {
        // 4 KiB RAM cannot hold even the header + a DTB.
        let backend = MockArm64Backend::new();
        assert!(compose(backend, &tiny_image(), "", 0x1000).is_err());
    }
}
