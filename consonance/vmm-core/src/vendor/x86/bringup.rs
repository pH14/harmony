// SPDX-License-Identifier: AGPL-3.0-or-later

use vmm_backend::{Backend, Gpa, X86, X86Policy};

use super::contract;
#[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
use super::entry;
#[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
use super::linux_loader::{self, LinuxImage};
#[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
use crate::vmm::GuestRam;
use crate::vmm::{RamBacking, Vmm, VmmError};
#[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
use vmm_backend::{CpuidModel, MpState, VcpuState};

#[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
const IA32_EFER: u32 = 0xC000_0080;

const LAPIC_TIMER_HZ: u64 = 24_000_000;
const BSP_APIC_ID: u32 = 0;

#[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
fn compose_linux_seeded<B: Backend<A = X86>>(
    mut backend: B,
    kernel: &[u8],
    initramfs: &[u8],
    guest_ram_len: usize,
    cmdline: &str,
    cpuid: CpuidModel,
    boot_seed: Option<&[u8; 64]>,
) -> Result<Vmm<B>, VmmError> {
    backend.set_policy(&X86Policy {
        cpuid,
        msr_filter: contract::msr_filter_allow(),
    })?;

    let mut ram = GuestRam::new(guest_ram_len)?;
    let image: LinuxImage = linux_loader::load(
        kernel,
        initramfs,
        guest_ram_len as u64,
        cmdline,
        ram.as_mut_bytes(),
    )
    .map_err(VmmError::vendor_boot)?;

    if let Some(seed) = boot_seed {
        linux_loader::write_rng_seed(ram.as_mut_bytes(), seed).map_err(VmmError::vendor_boot)?;
    }

    unsafe {
        backend.map_memory(Gpa(0), ram.as_mut_bytes())?;
    }

    let entry_state = entry::long_mode_entry(
        image.entry_point,
        image.boot_params_gpa,
        image.page_table_root,
        image.gdt_gpa,
    );
    let mut state = backend.save()?;
    apply_linux_entry(&mut state, &entry_state);
    backend.restore(&state)?;

    let lapic = lapic::Lapic::new(lapic::LapicConfig {
        apic_id: BSP_APIC_ID,
        timer_hz: LAPIC_TIMER_HZ,
    })
    .map_err(|e| VmmError::ContractViolation(format!("lapic init: {e}")))?;

    let mut vmm = Vmm::new(backend, ram);
    vmm.wire_lapic(lapic);
    Ok(vmm)
}

pub fn compose_restore_target<B: Backend<A = X86>>(
    backend: B,
    mapping: snapshot_store::Mapping,
    wire_lapic: bool,
) -> Result<Vmm<B>, VmmError> {
    compose_restore_target_with_policy(
        backend,
        mapping,
        wire_lapic,
        X86Policy {
            cpuid: contract::cpuid_model(),
            msr_filter: contract::msr_filter_allow(),
        },
    )
}

fn compose_restore_target_with_policy<B: Backend<A = X86>>(
    mut backend: B,
    mut mapping: snapshot_store::Mapping,
    wire_lapic: bool,
    policy: X86Policy,
) -> Result<Vmm<B>, VmmError> {
    backend.set_policy(&policy)?;

    if mapping.is_empty() || !mapping.len().is_multiple_of(4096) {
        return Err(VmmError::Backend(vmm_backend::BackendError::Memory(
            "snapshot mapping length must be a non-zero multiple of 4 KiB",
        )));
    }
    unsafe {
        backend.map_memory(Gpa(0), mapping.as_mut_slice())?;
    }

    let mut vmm = Vmm::with_backing(backend, RamBacking::Snapshot(mapping));
    if wire_lapic {
        let lapic = lapic::Lapic::new(lapic::LapicConfig {
            apic_id: BSP_APIC_ID,
            timer_hz: LAPIC_TIMER_HZ,
        })
        .map_err(|e| VmmError::ContractViolation(format!("lapic init: {e}")))?;
        vmm.wire_lapic(lapic);
    }
    Ok(vmm)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn compose_stock_virtual_time_restore_target(
    mapping: snapshot_store::Mapping,
    seed: u64,
) -> Result<Vmm<Box<dyn Backend<A = X86>>>, VmmError> {
    let backend: Box<dyn Backend<A = X86>> = Box::new(vmm_backend::KvmBackend::new()?);
    let mut vmm = compose_restore_target_with_policy(
        backend,
        mapping,
        true,
        X86Policy {
            cpuid: contract::cpuid_model(),
            msr_filter: contract::msr_filter_allow(),
        },
    )?;
    vmm.wire_vtime(crate::vmm::VtimeWiring::new_virtual_time(
        super::contract_vclock_config(),
        seed,
    )?);
    vmm.enable_pvclock();
    Ok(vmm)
}

#[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
fn apply_linux_entry(state: &mut VcpuState, entry: &VcpuState) {
    state.regs = entry.regs;
    state.sregs.cs = entry.sregs.cs;
    state.sregs.ds = entry.sregs.ds;
    state.sregs.es = entry.sregs.es;
    state.sregs.fs = entry.sregs.fs;
    state.sregs.gs = entry.sregs.gs;
    state.sregs.ss = entry.sregs.ss;
    state.sregs.gdt = entry.sregs.gdt;
    state.sregs.cr0 = entry.sregs.cr0;
    state.sregs.cr2 = entry.sregs.cr2;
    state.sregs.cr3 = entry.sregs.cr3;
    state.sregs.cr4 = entry.sregs.cr4;
    state.sregs.efer = entry.sregs.efer;
    state.msrs.insert(IA32_EFER, entry.sregs.efer);
    state.mp_state = MpState::Runnable;
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn boot_linux_stock_virtual_time(
    kernel: &[u8],
    initramfs: &[u8],
    guest_ram_len: usize,
    cmdline: &str,
    seed: u64,
) -> Result<Vmm<Box<dyn Backend<A = X86>>>, VmmError> {
    let backend: Box<dyn Backend<A = X86>> = Box::new(vmm_backend::KvmBackend::new()?);
    compose_linux_virtual_time(backend, kernel, initramfs, guest_ram_len, cmdline, seed)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn boot_linux_stock_virtual_time_cancellable(
    kernel: &[u8],
    initramfs: &[u8],
    guest_ram_len: usize,
    cmdline: &str,
    seed: u64,
) -> Result<Vmm<vmm_backend::KvmBackend>, VmmError> {
    compose_linux_virtual_time(
        vmm_backend::KvmBackend::new()?,
        kernel,
        initramfs,
        guest_ram_len,
        cmdline,
        seed,
    )
}

#[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
fn compose_linux_virtual_time<B: Backend<A = X86>>(
    backend: B,
    kernel: &[u8],
    initramfs: &[u8],
    guest_ram_len: usize,
    cmdline: &str,
    seed: u64,
) -> Result<Vmm<B>, VmmError> {
    let mut wiring =
        crate::vmm::VtimeWiring::new_virtual_time(super::contract_vclock_config(), seed)?;
    let mut boot_seed = [0u8; 64];
    for chunk in boot_seed.chunks_exact_mut(8) {
        chunk.copy_from_slice(&wiring.next_entropy_word()?.to_le_bytes());
    }
    let mut vmm = compose_linux_seeded(
        backend,
        kernel,
        initramfs,
        guest_ram_len,
        cmdline,
        contract::cpuid_model(),
        Some(&boot_seed),
    )?;
    vmm.wire_vtime(wiring);
    vmm.enable_pvclock();
    Ok(vmm)
}

#[cfg(test)]
mod tests {

    use std::cell::Cell;
    use std::rc::Rc;

    use vmm_backend::{CommonExit, Exit, MockBackend};

    use super::*;
    use crate::vmm::TerminalReason;

    struct PtrRetainingBackend {
        inner: MockBackend,
        mapped: RetainedRegion,
    }

    type RetainedRegion = Rc<Cell<Option<(*mut u8, usize)>>>;

    impl PtrRetainingBackend {
        fn new() -> (Self, RetainedRegion) {
            let mapped = Rc::new(Cell::new(None));
            (
                Self {
                    inner: MockBackend::new(),
                    mapped: Rc::clone(&mapped),
                },
                mapped,
            )
        }
    }

    impl Backend for PtrRetainingBackend {
        type A = vmm_backend::X86;

        fn set_policy(&mut self, policy: &vmm_backend::X86Policy) -> vmm_backend::Result<()> {
            self.inner.set_policy(policy)
        }
        unsafe fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> vmm_backend::Result<()> {
            // SAFETY: forwarded under the caller's own `map_memory` contract.
            let res = unsafe { self.inner.map_memory(gpa, host) };
            self.mapped.set(Some((host.as_mut_ptr(), host.len())));
            res
        }
        fn run(&mut self) -> vmm_backend::Result<Exit<vmm_backend::X86>> {
            self.inner.run()
        }
        fn inject(&mut self, event: vmm_backend::Injection) -> vmm_backend::Result<()> {
            self.inner.inject(event)
        }
        fn set_pending_irq(&mut self, vector: Option<u8>) -> vmm_backend::Result<()> {
            self.inner.set_pending_irq(vector)
        }
        fn take_accepted_interrupt(&mut self) -> Option<u8> {
            self.inner.take_accepted_interrupt()
        }
        fn complete_read(&mut self, value: u64) -> vmm_backend::Result<()> {
            self.inner.complete_read(value)
        }
        fn complete_fault(&mut self) -> vmm_backend::Result<()> {
            self.inner.complete_fault()
        }
        fn complete_ok(&mut self) -> vmm_backend::Result<()> {
            self.inner.complete_ok()
        }
        fn complete_hypercall(&mut self, rax: u64) -> vmm_backend::Result<()> {
            self.inner.complete_hypercall(rax)
        }
        fn complete_arch(&mut self, c: vmm_backend::X86Completion) -> vmm_backend::Result<()> {
            self.inner.complete_arch(c)
        }
        fn save(&self) -> vmm_backend::Result<VcpuState> {
            self.inner.save()
        }
        fn restore(&mut self, state: &VcpuState) -> vmm_backend::Result<()> {
            self.inner.restore(state)
        }
        fn exit_counts(&self) -> vmm_backend::ExitCounts {
            self.inner.exit_counts()
        }
        fn reset_exit_counts(&mut self) {
            self.inner.reset_exit_counts()
        }
        fn capabilities(&self) -> vmm_backend::Capabilities<vmm_backend::X86Caps> {
            self.inner.capabilities()
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "materialize's tempfile+mmap can't run under Miri; the unsafe map_memory over the \
                  Mapping is exercised under Miri by compose_restore_target_map_memory_over_an_anonymous_mapping"
    )]
    fn compose_restore_target_maps_the_snapshot_without_loading() {
        let mut store = snapshot_store::Store::new(snapshot_store::StoreConfig { mem_pages: 4 });
        let mut base = store.begin_base();
        let mut page = vec![0u8; 4096];
        page[..4].copy_from_slice(b"SNAP");
        base.write_page(2, &page).unwrap();
        let id = base.seal(b"vm".to_vec());
        let mapping = store.materialize(id).unwrap();

        let vmm = compose_restore_target(MockBackend::new(), mapping, true).unwrap();
        assert!(vmm.ram_backing_is_snapshot());
        assert!(vmm.lapic_wired());
        let mem = vmm.guest_memory();
        assert_eq!(mem.len(), 4 * 4096);
        assert_eq!(&mem[2 * 4096..2 * 4096 + 4], b"SNAP");
        assert!(mem[..4096].iter().all(|&b| b == 0), "no loader ever ran");
    }

    #[test]
    fn compose_restore_target_map_memory_over_an_anonymous_mapping() {
        const PAGES: usize = 4;
        let mut mapping = snapshot_store::Mapping::anonymous(PAGES * 4096);
        mapping.as_mut_slice()[2 * 4096..2 * 4096 + 4].copy_from_slice(b"SNAP");

        let (backend, mapped) = PtrRetainingBackend::new();
        let vmm = compose_restore_target(backend, mapping, true).unwrap();

        let (base, len) = mapped.get().expect("map_memory was called");
        assert_eq!(len, PAGES * 4096);
        assert_eq!(
            base as usize % 4096,
            0,
            "map_memory contract (c): the host base address must be 4 KiB-aligned"
        );
        // SAFETY: contract (a) — the `Vmm` owns the mapping and keeps its heap
        // buffer live at a fixed address; nothing has re-borrowed the buffer since
        // `map_memory`, and no run is in flight, so the retained pointer is valid
        // for this read. Under Miri this is the checked obligation, not an
        // assumption.
        let through_backend = unsafe { std::slice::from_raw_parts(base, len) };
        assert_eq!(
            &through_backend[2 * 4096..2 * 4096 + 4],
            b"SNAP",
            "the retained backend pointer reads the moved mapping's bytes"
        );
        assert!(
            through_backend[..4096].iter().all(|&b| b == 0),
            "no loader ever ran"
        );

        assert!(
            vmm.ram_backing_is_snapshot(),
            "the mapping itself is the guest RAM backing"
        );
        assert!(vmm.lapic_wired());
        let mem = vmm.guest_memory();
        assert_eq!(mem.len(), PAGES * 4096);
        assert_eq!(
            &mem[2 * 4096..2 * 4096 + 4],
            b"SNAP",
            "the mapped buffer reads through as guest RAM"
        );
    }

    fn synthetic_bzimage(pref_address: u32, pm_len: usize) -> Vec<u8> {
        let pm_off = (1 + 1) * 512usize;
        let mut img = vec![0u8; pm_off + pm_len];
        img[0x1f1] = 1;
        img[0x1fe..0x200].copy_from_slice(&0xAA55u16.to_le_bytes());
        img[0x202..0x206].copy_from_slice(&0x5372_6448u32.to_le_bytes());
        img[0x206..0x208].copy_from_slice(&0x020fu16.to_le_bytes());
        img[0x22c..0x230].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
        img[0x236..0x238].copy_from_slice(&1u16.to_le_bytes());
        img[0x238..0x23c].copy_from_slice(&0x7FFu32.to_le_bytes());
        img[0x258..0x260].copy_from_slice(&u64::from(pref_address).to_le_bytes());
        for (i, b) in img[pm_off..].iter_mut().enumerate() {
            *b = (0x11 + (i % 0x40)) as u8;
        }
        img
    }

    #[test]
    fn virtual_time_boot_seeds_linux_from_the_shared_entropy_stream() {
        use hypercall_proto::Service;
        let kernel = synthetic_bzimage(0x10_0000, 0x400);
        let boot = |seed| {
            compose_linux_virtual_time(
                MockBackend::new(),
                &kernel,
                &[],
                0x20_0000,
                "console=ttyS0",
                seed,
            )
            .unwrap()
        };
        let a = boot(7);
        let b = boot(7);
        let c = boot(8);
        assert_eq!(a.guest_memory(), b.guest_memory());
        assert_ne!(
            &a.guest_memory()[0x9010..0x9050],
            &c.guest_memory()[0x9010..0x9050]
        );
        let mut expected =
            crate::vmm::VtimeWiring::new_virtual_time(super::super::contract_vclock_config(), 7)
                .unwrap();
        let mut bytes = Vec::new();
        for _ in 0..8 {
            bytes.extend_from_slice(&expected.next_entropy_word().unwrap().to_le_bytes());
        }
        assert_eq!(&a.guest_memory()[0x9010..0x9050], bytes.as_slice());
        assert_eq!(
            a.vtime.as_ref().unwrap().entropy.save_state(),
            expected.entropy.save_state()
        );
    }

    #[test]
    #[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
    fn apply_linux_entry_overlays_long_mode_state_gdtr_and_efer_msr() {
        let entry = entry::long_mode_entry(0x10_0200, 0x7000, 0x1000, 0x6000);
        let mut state = VcpuState::default();
        apply_linux_entry(&mut state, &entry);
        assert_eq!(state.regs.rip, 0x10_0200);
        assert_eq!(state.regs.rsi, 0x7000);
        assert_eq!(state.sregs.cs.selector, 0x10);
        assert_eq!(state.sregs.cs.l, 1);
        assert_eq!(state.sregs.cr3, 0x1000);
        assert_eq!(state.sregs.cr0, entry.sregs.cr0);
        assert_eq!(state.sregs.cr4, entry.sregs.cr4);
        assert_eq!(state.sregs.efer, entry.sregs.efer);
        assert_eq!(state.sregs.gdt.base, 0x6000);
        assert_eq!(state.msrs.get(&0xC000_0080), Some(&entry.sregs.efer));
        assert!(matches!(state.mp_state, MpState::Runnable));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "compose_linux needs >1 MiB guest RAM (pref_address); the unsafe map seam is \
                  covered under Miri by compose_drives_guestram_and_unsafe_map_memory"
    )]
    fn compose_linux_loads_kernel_and_wires_lapic() {
        let kernel = synthetic_bzimage(0x10_0000, 0x400);
        let backend = MockBackend::with_exits(vec![Exit::Common(CommonExit::Idle)]);
        let ram = 0x20_0000usize;
        let mut vmm = compose_linux_seeded(
            backend,
            &kernel,
            &[],
            ram,
            "console=ttyS0",
            contract::cpuid_model(),
            None,
        )
        .expect("compose_linux");

        assert!(vmm.lapic_wired());
        let r = vmm.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Idle);

        let blob = vmm.state_blob().unwrap();
        let mem = &blob[12..12 + ram];
        assert_eq!(mem[0x10_0000], 0x11, "kernel copied to pref_address");
        assert_eq!(
            u32::from_le_bytes(mem[0x7202..0x7206].try_into().unwrap()),
            0x5372_6448,
            "boot_params.hdr.header == HdrS"
        );
    }
}
