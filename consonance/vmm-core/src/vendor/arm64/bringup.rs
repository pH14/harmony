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
    mut backend: B,
    image: &[u8],
    bootargs: &str,
    guest_ram_len: usize,
) -> Result<Vmm<B>, VmmError> {
    // 1. Install the contract policy skeleton through the trait, before the
    //    first run (the arm64 `ID_AA64*` freeze + trapped-sysreg table; rows
    //    TODO(AA-6)).
    backend.set_policy(&contract::policy())?;

    // 2. Allocate RAM and flat-load the Image.
    let mut ram = GuestRam::new(guest_ram_len)?;
    let loaded = image_loader::load(image, ram.as_mut_bytes()).map_err(VmmError::vendor_boot)?;

    // 3. Place the DTB in RAM immediately above the loaded image, page-aligned.
    //    (The reserved pvclock page sits one page above the DTB — the hm-rk5
    //    seam, reserved and named in the DTB, populated by that bead.)
    let dtb_off = align_up(loaded.end_off, PAGE);
    let dtb_gpa = RAM_BASE + dtb_off;
    // The pvclock page is reserved a fixed page above the DTB; the DTB names
    // it. It is sized before the DTB is built (its GPA does not depend on the
    // DTB length, only on a fixed reservation above it).
    let dtb_bytes = dtb::build(guest_ram_len as u64, 0, bootargs);
    let pvclock_gpa = RAM_BASE + align_up(dtb_off + dtb_bytes.len() as u64, PAGE);
    // Rebuild with the now-known pvclock GPA so the reserved-memory node names
    // the real page (the length is unchanged — the GPA is a fixed-width field).
    let dtb_bytes = dtb::build(guest_ram_len as u64, pvclock_gpa, bootargs);

    let dtb_end = dtb_off as usize + dtb_bytes.len();
    let ram_bytes = ram.as_mut_bytes();
    if dtb_end > ram_bytes.len()
        || (pvclock_gpa - RAM_BASE) as usize + PAGE as usize > ram_bytes.len()
    {
        return Err(VmmError::ContractViolation(format!(
            "arm64 boot: image + DTB + reserved pvclock page do not fit in {guest_ram_len:#x} \
             bytes of guest RAM (DTB ends at {dtb_end:#x}, pvclock page at \
             {:#x})",
            pvclock_gpa - RAM_BASE
        )));
    }
    ram_bytes[dtb_off as usize..dtb_end].copy_from_slice(&dtb_bytes);

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

    // 6. Hand the configured backend + owned RAM to the Vmm.
    Ok(Vmm::new(backend, ram))
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
        // The reserved pvclock page GPA the DTB names is real, page-aligned RAM.
        let pv = parsed.prop("pvclock@0", "reg").unwrap();
        let pv_gpa = u64::from_be_bytes(pv[0..8].try_into().unwrap());
        assert!(pv_gpa.is_multiple_of(PAGE));
        assert!(pv_gpa >= RAM_BASE && pv_gpa < RAM_BASE + ram_len as u64);
    }

    #[test]
    fn compose_rejects_an_image_that_does_not_fit() {
        // 4 KiB RAM cannot hold even the header + a DTB.
        let backend = MockArm64Backend::new();
        assert!(compose(backend, &tiny_image(), "", 0x1000).is_err());
    }
}
