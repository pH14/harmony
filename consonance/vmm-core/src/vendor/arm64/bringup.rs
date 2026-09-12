// SPDX-License-Identifier: AGPL-3.0-or-later

use vmm_backend::{Arm64, Backend, Gpa};

use super::board::{PAGE, RAM_BASE, align_up};
use super::{contract, dtb, entry, image_loader};
use crate::vmm::{GuestRam, Vmm, VmmError};

pub fn boot<B: Backend<A = Arm64>>(
    backend: B,
    image: &[u8],
    bootargs: &str,
    guest_ram_len: usize,
) -> Result<Vmm<B>, VmmError> {
    compose(backend, image, bootargs, guest_ram_len)
}

pub(crate) fn compose<B: Backend<A = Arm64>>(
    backend: B,
    image: &[u8],
    bootargs: &str,
    guest_ram_len: usize,
) -> Result<Vmm<B>, VmmError> {
    compose_inner(backend, image, None, bootargs, guest_ram_len, true)
}

fn compose_inner<B: Backend<A = Arm64>>(
    mut backend: B,
    image: &[u8],
    initramfs: Option<&[u8]>,
    bootargs: &str,
    guest_ram_len: usize,
    map_doorbell: bool,
) -> Result<Vmm<B>, VmmError> {
    backend.set_policy(&contract::policy())?;

    let mut ram = GuestRam::new(guest_ram_len)?;
    let loaded = image_loader::load(image, ram.as_mut_bytes()).map_err(VmmError::vendor_boot)?;

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
    let initrd_end = initrd_layout.map(|(_, end, _, _)| end);
    if !layout_fits(dtb_end, pvclock_end, initrd_end, ram_bytes.len(), ram_len) {
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

    unsafe {
        backend.map_memory(Gpa(RAM_BASE), ram.as_mut_bytes())?;
    }

    let entry_state = entry::boot_entry(loaded.entry_gpa, dtb_gpa);
    let mut state = backend.save()?;
    entry::apply_entry(&mut state, &entry_state);
    backend.restore(&state)?;

    let mut vmm = Vmm::new(backend, ram);
    vmm.ram_base_gpa = RAM_BASE;
    if map_doorbell {
        vmm.map_doorbell_pages()?;
    }
    Ok(vmm)
}

fn layout_fits(
    dtb_end: usize,
    pvclock_end: u64,
    initrd_end: Option<u64>,
    ram_bytes_len: usize,
    ram_len: u64,
) -> bool {
    dtb_end <= ram_bytes_len
        && pvclock_end <= ram_len
        && initrd_end.is_none_or(|end| end <= ram_len)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
pub fn boot_hvf(
    image: &[u8],
    initramfs: &[u8],
    bootargs: &str,
    guest_ram_len: usize,
) -> Result<Vmm<vmm_backend::HvfBackend>, VmmError> {
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
    vmm.wire_vtime(crate::vmm::VtimeWiring::new_virtual_time(
        vtime::VClockConfig {
            guest_hz: super::board::CNTFRQ_HZ,
            guest_base: 0,
            vns_base: 0,
        },
        0,
    )?);
    vmm.enable_pvclock();
    Ok(vmm)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
pub fn boot_hvf_control(
    image: &[u8],
    initramfs: &[u8],
    bootargs: &str,
    guest_ram_len: usize,
) -> Result<Vmm<vmm_backend::HvfBackend>, VmmError> {
    let backend = vmm_backend::HvfBackend::new()?;
    let mut vmm = compose_inner(
        backend,
        image,
        Some(initramfs),
        bootargs,
        guest_ram_len,
        true,
    )?;
    vmm.wire_gic(super::board::new_gic());
    vmm.wire_vtime(crate::vmm::VtimeWiring::new_virtual_time(
        vtime::VClockConfig {
            guest_hz: super::board::CNTFRQ_HZ,
            guest_base: 0,
            vns_base: 0,
        },
        0,
    )?);
    vmm.enable_pvclock();
    Ok(vmm)
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub fn boot_selected(
    image: &[u8],
    initramfs: &[u8],
    bootargs: &str,
    guest_ram_len: usize,
) -> Result<Vmm<Box<dyn Backend<A = Arm64>>>, VmmError> {
    boot_selected_inner(image, initramfs, bootargs, guest_ram_len, false)
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub fn boot_selected_control(
    image: &[u8],
    initramfs: &[u8],
    bootargs: &str,
    guest_ram_len: usize,
) -> Result<Vmm<Box<dyn Backend<A = Arm64>>>, VmmError> {
    boot_selected_inner(image, initramfs, bootargs, guest_ram_len, true)
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
fn boot_selected_inner(
    image: &[u8],
    initramfs: &[u8],
    bootargs: &str,
    guest_ram_len: usize,
    map_doorbell: bool,
) -> Result<Vmm<Box<dyn Backend<A = Arm64>>>, VmmError> {
    let live = vmm_backend::LiveKvm::new()?;
    let backend: Box<dyn Backend<A = Arm64>> = Box::new(vmm_backend::Arm64KvmBackend::new(live));
    let mut vmm = compose_inner(
        backend,
        image,
        Some(initramfs),
        bootargs,
        guest_ram_len,
        map_doorbell,
    )?;
    vmm.wire_vtime(crate::vmm::VtimeWiring::new_virtual_time(
        vtime::VClockConfig {
            guest_hz: super::board::CNTFRQ_HZ,
            guest_base: 0,
            vns_base: 0,
        },
        0,
    )?);
    vmm.enable_pvclock();
    Ok(vmm)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vmm_backend::MockArm64Backend;

    fn tiny_image() -> Vec<u8> {
        image_loader::wrap_image(&[0x42u8; 256], 0, 0xA)
    }

    #[test]
    fn compose_loads_image_places_dtb_and_sets_entry() {
        let ram_len = 16 * 1024 * 1024;
        let backend = MockArm64Backend::new();
        let vmm = compose(backend, &tiny_image(), "console=ttyAMA0", ram_len).unwrap();

        let vcpu = vmm.inspect_vcpu();
        assert_eq!(vcpu.core.pc, RAM_BASE);
        assert_eq!(vcpu.core.pstate, entry::PSTATE_EL1H_DAIF);
        let dtb_gpa = vcpu.core.x[0];
        assert!(dtb_gpa > RAM_BASE && dtb_gpa < RAM_BASE + ram_len as u64);

        let off = (dtb_gpa - RAM_BASE) as usize;
        let mem = vmm.guest_memory();
        let parsed = dtb::parse(&mem[off..]).unwrap();
        assert!(parsed.nodes.iter().any(|n| n == "pl011@9000000"));
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
        let backend = MockArm64Backend::new();
        assert!(compose(backend, &tiny_image(), "", 0x1000).is_err());
    }

    #[test]
    fn layout_fit_checks_each_extent_and_accepts_exact_boundaries() {
        assert!(layout_fits(10, 10, None, 10, 10));
        assert!(layout_fits(10, 10, Some(10), 10, 10));
        assert!(!layout_fits(11, 10, None, 10, 10));
        assert!(!layout_fits(10, 11, None, 10, 10));
        assert!(!layout_fits(10, 10, Some(11), 10, 10));
    }
}
