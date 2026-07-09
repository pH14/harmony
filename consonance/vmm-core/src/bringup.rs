// SPDX-License-Identifier: AGPL-3.0-or-later
//! Composition helper for the live run: allocate the owned [`GuestRam`], install
//! the contract policy **through the trait**, load the payload, write the
//! boot-info struct, build + restore the 32-bit-PM entry state, map the RAM, and
//! return a [`Vmm`] ready to `run()`.
//!
//! [`boot`] takes the `Backend` **by value** (constructed bare at the composition
//! root — e.g. `KvmBackend::new()` with no policy), so the only place a concrete
//! backend is named is the binary's `fn main` / the box integration test; policy
//! goes in through `set_cpuid`/`set_msr_filter`, not a concrete constructor.
//!
//! [`boot`] = the §1.1 host-baseline gate ([`crate::hostassert::enforce`]) **then**
//! [`compose`]. The split keeps the composition — including the `unsafe`
//! `map_memory` pointer seam — unit-testable with a mock backend on every platform
//! (and under Miri), independent of the box-only host gate.

use vmm_backend::{Backend, Gpa, MpState, VcpuState};

use crate::contract;
use crate::entry;
use crate::linux_loader::{self, LinuxImage};
use crate::multiboot;
use crate::vmm::{GuestRam, RamBacking, Vmm, VmmError};

/// `IA32_EFER` MSR index. `EFER` is an **allow-stateful** MSR, so the backend's
/// `restore` rewrites it from the snapshot's MSR map **after** `KVM_SET_SREGS2` —
/// overwriting the long-mode `EFER` the entry sregs carry. [`apply_linux_entry`]
/// therefore also sets it in the MSR map (else the guest enters with `LMA` but no
/// `LME` → VMX "invalid guest state", `KVM_EXIT_FAIL_ENTRY`).
const IA32_EFER: u32 = 0xC000_0080;

/// LAPIC-timer input frequency (Hz) the userspace xAPIC is configured with — the
/// frozen core-crystal clock (CPUID `0x15`) per `docs/CPU-MSR-CONTRACT.md` §5. The
/// exact value only governs the timer **deadline** arithmetic, which is moot until
/// interrupt injection lands (the bring-up `KvmBackend::inject` is `Unsupported`);
/// it is fixed here so the value is deterministic and documented. Non-zero
/// (required by [`lapic::Lapic::new`]).
const LAPIC_TIMER_HZ: u64 = 24_000_000;
/// The single vCPU is the BSP with APIC ID 0.
const BSP_APIC_ID: u32 = 0;

/// Which trap apparatus to run under (the composition-root selector, task-21 P5).
/// The *only* place a concrete backend is named is [`boot_selected`]; everything
/// above the `Backend` trait is backend-agnostic (R-Backend).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BackendKind {
    /// Stock KVM (`KvmBackend`) — bring-up default, **not** determinism-complete:
    /// RDTSC/RDTSCP/RDRAND/RDSEED are never surfaced (the declared holes).
    Stock,
    /// Patched KVM (`PatchedKvmBackend`) — the ratified determinism baseline:
    /// the four instruction exits are surfaced and resolved against V-time / the
    /// seeded entropy stream.
    Patched,
}

/// Allocate `guest_ram_len` of owned host-backed guest RAM, install the contract
/// policy via the trait, flat-load `payload`, write the boot-info struct, build +
/// restore the 32-bit-PM entry state, `unsafe`-map the RAM, and return a ready
/// [`Vmm`] that owns the backing. Pure-logic except `backend` calls — drivable
/// against the mock backend.
///
/// Order is load-bearing (tasks/15 §bringup): policy **before** the first run;
/// map **before** restore; `ram` moves into the `Vmm` so the mapped pointer stays
/// valid for the backend's lifetime.
pub fn boot<B: Backend>(
    backend: B,
    payload: &[u8],
    guest_ram_len: usize,
) -> Result<Vmm<B>, VmmError> {
    // 0. Enforce the CPU-MSR-CONTRACT §1.1 host-homogeneity baseline FIRST —
    //    before installing any policy or entering the guest. A host outside the
    //    frozen det-cfl-v1 determinism domain (wrong family/model/stepping or
    //    microcode, MXCSR-mask, MAXPHYADDR, an un-disabled RTM, or a variance
    //    instruction that should be absent) would diverge in native, non-trapping
    //    instruction/FPU behavior while still claiming the frozen contract, so we
    //    fail closed. No-op off the box (no physical guest there to protect).
    crate::hostassert::enforce()?;
    // 1-5. Compose the configured `Vmm` (separable from the host gate, so the
    //      composition — including the `unsafe` map seam — is unit-testable with a
    //      mock backend on every platform and under Miri).
    compose(backend, payload, guest_ram_len)
}

/// Compose a ready [`Vmm`] over `backend`, **without** the host-baseline gate:
/// install the contract policy via the trait, allocate the owned [`GuestRam`],
/// flat-load `payload`, write the boot-info struct, `unsafe`-map the RAM, and
/// build + restore the 32-bit-PM entry state. Split out of [`boot`] so the
/// composition (notably the `unsafe` `map_memory` pointer seam) is exercised by a
/// mock-backed unit test on **every** platform — including under Miri and on the
/// Linux box, where [`boot`] itself would refuse a non-baseline host before
/// reaching this code. Order is load-bearing (tasks/15 §bringup): policy **before**
/// the first run; map **before** restore; `ram` moves into the `Vmm` so the mapped
/// pointer stays valid for the backend's lifetime.
pub(crate) fn compose<B: Backend>(
    mut backend: B,
    payload: &[u8],
    guest_ram_len: usize,
) -> Result<Vmm<B>, VmmError> {
    // 1. Install policy through the trait, before the first run. The backend
    //    enables the USER_SPACE_MSR_MASK cap before the filter (below the trait).
    backend.set_cpuid(&contract::cpuid_model())?;
    backend.set_msr_filter(&contract::msr_filter_allow())?;

    // 2. Allocate RAM, flat-load the payload, write the minimal boot-info struct.
    let mut ram = GuestRam::new(guest_ram_len)?;
    let loaded = multiboot::load(payload, ram.as_mut_bytes())?;
    let mbi_gpa = entry::write_boot_info(ram.as_mut_bytes())?;

    // 3. Map the RAM into the backend; it retains a pointer into `ram`.
    // SAFETY (granted purpose 2): `ram` is moved into the returned `Vmm` in step 5
    // and its mmap/Vec heap does not move, so the pointer stays valid for the
    // backend's lifetime; the run loop holds `&mut self`, so the backing is never
    // aliased while a run is in flight; GuestRam's off-Miri backing is a
    // page-aligned `mmap` as KVM_SET_USER_MEMORY_REGION requires.
    unsafe {
        backend.map_memory(Gpa(0), ram.as_mut_bytes())?;
    }

    // 4. Build + restore the 32-bit-PM entry state. `restore` validates the XSAVE
    //    size and MSR key-set, which a pure builder cannot produce, so overlay the
    //    entry registers/segments/control-regs onto a live `save()` template that
    //    already carries the backend's valid TR/LDT/GDT/IDT/XSAVE/MSR shape (this
    //    mirrors the proven get→modify→set pattern of a working stock-KVM VMM).
    let entry_state = entry::protected_mode_entry(loaded.entry_addr, mbi_gpa);
    let mut state = backend.save()?;
    apply_entry(&mut state, &entry_state);
    backend.restore(&state)?;

    // 5. Hand the configured backend + owned RAM to the Vmm.
    Ok(Vmm::new(backend, ram))
}

/// Overlay the Multiboot entry registers/segments/control-regs onto a backend
/// `save()` template, keeping the template's valid `TR`/`LDT`/`GDT`/`IDT`/
/// `apic_base`/XSAVE/MSR shape (which `restore` validates).
fn apply_entry(state: &mut VcpuState, entry: &VcpuState) {
    state.regs = entry.regs;
    state.sregs.cs = entry.sregs.cs;
    state.sregs.ds = entry.sregs.ds;
    state.sregs.es = entry.sregs.es;
    state.sregs.fs = entry.sregs.fs;
    state.sregs.gs = entry.sregs.gs;
    state.sregs.ss = entry.sregs.ss;
    state.sregs.cr0 = entry.sregs.cr0;
    state.sregs.cr2 = entry.sregs.cr2;
    state.sregs.cr3 = entry.sregs.cr3;
    state.sregs.cr4 = entry.sregs.cr4;
    state.sregs.efer = entry.sregs.efer;
    state.mp_state = MpState::Runnable;
}

// ---------------------------------------------------------------------------
// Linux direct 64-bit boot (task 30).
// ---------------------------------------------------------------------------

/// Which image format a payload is, for the [`bringup`](crate::bringup) dispatch:
/// a Multiboot v1 kernel (the task-04 payloads) or a Linux bzImage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImageKind {
    /// A Multiboot v1 image ([`multiboot::load`] / [`boot`]).
    Multiboot,
    /// A Linux bzImage ([`linux_loader::load`] / [`boot_linux`]).
    Linux,
}

impl ImageKind {
    /// Classify `image`: a valid bzImage `setup_header` (`boot_flag`/`HdrS`) ⇒
    /// [`ImageKind::Linux`]; a Multiboot v1 header magic in the first 8 KiB ⇒
    /// [`ImageKind::Multiboot`]; otherwise `None`. Pure; never panics on arbitrary
    /// bytes. The Linux check is tried first (its two fixed magics are far more
    /// specific than the scanned Multiboot magic, so they cannot be confused).
    pub fn detect(image: &[u8]) -> Option<ImageKind> {
        if linux_loader::parse_setup_header(image).is_ok() {
            Some(ImageKind::Linux)
        } else if multiboot::parse_header(image).is_ok() {
            Some(ImageKind::Multiboot)
        } else {
            None
        }
    }
}

/// Boot a Linux bzImage + initramfs via the **direct 64-bit boot protocol**: the
/// §1.1 host-baseline gate ([`crate::hostassert::enforce`]) **then**
/// [`compose_linux`]. Mirrors [`boot`] for the Multiboot path.
pub fn boot_linux<B: Backend>(
    backend: B,
    kernel: &[u8],
    initramfs: &[u8],
    guest_ram_len: usize,
    cmdline: &str,
) -> Result<Vmm<B>, VmmError> {
    crate::hostassert::enforce()?;
    compose_linux(backend, kernel, initramfs, guest_ram_len, cmdline)
}

/// Compose a ready [`Vmm`] for a Linux direct 64-bit boot, **without** the
/// host-baseline gate (so the composition — including the `unsafe` `map_memory`
/// seam and the loader — is unit-testable with a mock backend on every platform).
/// Mirrors [`compose`]: install the contract policy, allocate RAM,
/// [`linux_loader::load`] the kernel/initramfs/`boot_params`/page-tables/GDT, map
/// the RAM, build + restore the long-mode entry state, and wire the userspace
/// xAPIC. Order is load-bearing: policy **before** the first run; map **before**
/// restore; `ram` moves into the `Vmm` so the mapped pointer stays valid.
pub(crate) fn compose_linux<B: Backend>(
    mut backend: B,
    kernel: &[u8],
    initramfs: &[u8],
    guest_ram_len: usize,
    cmdline: &str,
) -> Result<Vmm<B>, VmmError> {
    // 1. Install policy through the trait, before the first run.
    backend.set_cpuid(&contract::cpuid_model())?;
    backend.set_msr_filter(&contract::msr_filter_allow())?;

    // 2. Allocate RAM and flat-load the kernel + initramfs + boot_params + page
    //    tables + GDT (the loader is total over the untrusted image bytes).
    let mut ram = GuestRam::new(guest_ram_len)?;
    let image: LinuxImage = linux_loader::load(
        kernel,
        initramfs,
        guest_ram_len as u64,
        cmdline,
        ram.as_mut_bytes(),
    )?;

    // 3. Map the RAM into the backend; it retains a pointer into `ram`.
    // SAFETY (granted purpose 2): identical to `compose` — `ram` is moved into the
    // returned `Vmm` (step 6) and its backing never moves, so the pointer stays
    // valid for the backend's lifetime; the run loop holds `&mut self`, so the
    // backing is never aliased while a run is in flight; the off-Miri backing is a
    // page-aligned `mmap` as `KVM_SET_USER_MEMORY_REGION` requires.
    unsafe {
        backend.map_memory(Gpa(0), ram.as_mut_bytes())?;
    }

    // 4. Build + restore the long-mode entry state, overlaid onto a live `save()`
    //    template (keeping KVM's valid TR/LDT/XSAVE/MSR shape — same pattern as
    //    the Multiboot path), plus the GDTR the protocol requires.
    let entry_state = entry::long_mode_entry(
        image.entry_point,
        image.boot_params_gpa,
        image.page_table_root,
        image.gdt_gpa,
    );
    let mut state = backend.save()?;
    apply_linux_entry(&mut state, &entry_state);
    backend.restore(&state)?;

    // 5. Wire the userspace xAPIC (the kernel touches the `0xFEE0_0000` page
    //    early). `Lapic::new` only fails on a zero `timer_hz`, which the constant
    //    is not — but propagate rather than unwrap (rule #4).
    let lapic = lapic::Lapic::new(lapic::LapicConfig {
        apic_id: BSP_APIC_ID,
        timer_hz: LAPIC_TIMER_HZ,
    })
    .map_err(|e| VmmError::ContractViolation(format!("lapic init: {e}")))?;

    // 6. Hand the configured backend + owned RAM to the Vmm and wire the xAPIC.
    let mut vmm = Vmm::new(backend, ram);
    vmm.wire_lapic(lapic);
    Ok(vmm)
}

/// Compose a **restore target around a materialized snapshot** (task 95 M2.2,
/// the memslot-remap restore): install the contract policy, then `unsafe`-map
/// the [`snapshot_store::Mapping`]'s buffer as the guest RAM itself — no
/// [`GuestRam`] allocation, no image load, no entry state, and **no memcpy**.
/// The returned [`Vmm`] owns the mapping ([`RamBacking::Snapshot`]); the caller
/// completes it exactly like its normal factory target — wire the xAPIC iff the
/// snapshot source had one (`wire_lapic: true` for the Linux composition),
/// V-time ([`Vmm::wire_vtime`]) and hash opt-ins as usual — then restores the
/// non-memory half with [`Vmm::restore_vm_state`] (NOT `restore_snapshot`: the
/// memory half is already in place).
///
/// Boot-time image loading is deliberately skipped: the mapping already holds
/// the snapshot's memory, and a loader write would clobber it (privately — CoW
/// keeps the store safe — but wrongly). The vCPU needs no entry state either:
/// `restore_vm_state` overwrites the complete register file from the snapshot.
/// `MAP_PRIVATE` does the rest — guest writes stay private to this VM, and
/// untouched pages fault lazily instead of being copied eagerly.
pub fn compose_restore_target<B: Backend>(
    mut backend: B,
    mut mapping: snapshot_store::Mapping,
    wire_lapic: bool,
) -> Result<Vmm<B>, VmmError> {
    // 1. Install policy through the trait, before the first run (same order as
    //    `compose`/`compose_linux`: policy precedes any entry).
    backend.set_cpuid(&contract::cpuid_model())?;
    backend.set_msr_filter(&contract::msr_filter_allow())?;

    // 2. The mapping IS the guest RAM. `map_memory` requires page-aligned length
    //    (the mmap base is page-aligned by construction); a store sized per
    //    `SnapshotEngine` always satisfies this — reject a hand-rolled misuse.
    if mapping.is_empty() || !mapping.len().is_multiple_of(4096) {
        return Err(VmmError::Backend(vmm_backend::BackendError::Memory(
            "snapshot mapping length must be a non-zero multiple of 4 KiB",
        )));
    }
    // SAFETY (granted purpose 2): identical argument to `compose` — the mapping
    // is moved into the returned `Vmm` (step 3) and its mmap pages never move
    // when the owning struct does, so the pointer stays valid for the backend's
    // lifetime; the run loop holds `&mut self`, so the backing is never aliased
    // while a run is in flight; the buffer is an `mmap` (page-aligned) as
    // `KVM_SET_USER_MEMORY_REGION` requires.
    unsafe {
        backend.map_memory(Gpa(0), mapping.as_mut_slice())?;
    }

    // 3. Hand the configured backend + owned mapping to the Vmm; mirror the
    //    snapshot source's device composition so `restore_vm_state`'s wiring
    //    check passes (the restored LAPIC state replaces this fresh one wholesale).
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

/// Overlay the long-mode entry registers/segments/control-regs **and the GDTR**
/// onto a backend `save()` template, keeping the template's valid
/// `TR`/`LDT`/`IDT`/`apic_base`/XSAVE/MSR shape. Like [`apply_entry`] but also
/// copies `gdt` (the 64-bit boot protocol requires `GDTR` to point at the boot
/// GDT the loader wrote) — so it is a separate function, leaving the proven
/// Multiboot overlay untouched.
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
    // EFER is allow-stateful: the backend's `restore` rewrites it from this MSR map
    // *after* SET_SREGS2, so the long-mode EFER must live here too or it is clobbered
    // back to the reset value (LMA without LME → VMX invalid guest state). The key
    // already exists in the `save()` template (EFER is in the allow-stateful set), so
    // this overwrites its value without changing the validated key set.
    state.msrs.insert(IA32_EFER, entry.sregs.efer);
    state.mp_state = MpState::Runnable;
}

/// The **composition root** (task-21 P5): select the backend by [`BackendKind`],
/// inject it as a `Box<dyn Backend>`, [`boot`] over it, and — for
/// [`BackendKind::Patched`] — wire the determinism-complete V-time + seeded-RNG
/// path (a box-only `perf_event` work counter + the contract clock + `seed`). The
/// one place `KvmBackend`/`PatchedKvmBackend` are named; the returned
/// `Vmm<Box<dyn Backend>>` is otherwise backend-agnostic, so a `fn main` (or the
/// box integration test) drives either substrate through the same type. `seed` is
/// ignored for [`BackendKind::Stock`] (it surfaces no RNG).
///
/// Box-only (`#[cfg(target_os = "linux")]`): the concrete backends and the
/// `perf_event` counter need bare-metal KVM. On macOS the determinism path is
/// exercised via the scripted `MockBackend` + `ScriptedWork` unit tests instead.
#[cfg(target_os = "linux")]
pub fn boot_selected(
    kind: BackendKind,
    payload: &[u8],
    guest_ram_len: usize,
    seed: u64,
) -> Result<Vmm<Box<dyn Backend>>, VmmError> {
    match kind {
        BackendKind::Stock => {
            let backend: Box<dyn Backend> = Box::new(vmm_backend::KvmBackend::new()?);
            boot(backend, payload, guest_ram_len)
        }
        BackendKind::Patched => {
            let backend: Box<dyn Backend> = Box::new(vmm_backend::PatchedKvmBackend::new()?);
            let mut vmm = boot(backend, payload, guest_ram_len)?;
            // V-time work source: the guest-only retired-branch perf counter on
            // the (CPU-pinned) vCPU thread. Computed above the trait; the backend
            // never reads it.
            let work = Box::new(crate::work_perf::PerfWorkCounter::open()?);
            let wiring =
                crate::vmm::VtimeWiring::new(crate::vmm::contract_vclock_config(), work, seed)?;
            vmm.wire_vtime(wiring);
            Ok(vmm)
        }
    }
}

/// The Linux composition root (box-only): select the backend by [`BackendKind`],
/// [`boot_linux`] over it, and — for [`BackendKind::Patched`] — wire the
/// determinism-complete V-time + seeded-RNG path (so the xAPIC timer and any RDTSC
/// the kernel reads resolve to V-time). Mirrors [`boot_selected`] for the
/// Multiboot payloads; the box live-boot / determinism gates drive either
/// substrate through the same returned `Vmm<Box<dyn Backend>>`.
#[cfg(target_os = "linux")]
pub fn boot_linux_selected(
    kind: BackendKind,
    kernel: &[u8],
    initramfs: &[u8],
    guest_ram_len: usize,
    cmdline: &str,
    seed: u64,
) -> Result<Vmm<Box<dyn Backend>>, VmmError> {
    // V-time is wired on **both** substrates here, because the frozen contract
    // marks IA32_TSC (0x10) and IA32_TSC_ADJUST (0x3b) `emulate-vtime` and Linux
    // reads them early in boot — so even the stock boot must service those MSR
    // exits from V-time (BRINGUP: "Linux reads RDTSC/RDRAND early … determinism is
    // defined to require the patched path"). The substrates differ in what *else*
    // is deterministic: on `Patched` the RDTSC/RDRAND **instructions** also trap to
    // V-time / the seeded stream (fully deterministic — Phase C); on `Stock` those
    // instructions still execute in-guest against the host TSC/RNG (untrapped, so
    // the boot is nondeterministic by construction — Phase A only *proves the
    // boot*, it claims no determinism).
    let mut vmm = match kind {
        BackendKind::Stock => {
            let backend: Box<dyn Backend> = Box::new(vmm_backend::KvmBackend::new()?);
            boot_linux(backend, kernel, initramfs, guest_ram_len, cmdline)?
        }
        BackendKind::Patched => {
            let backend: Box<dyn Backend> = Box::new(vmm_backend::PatchedKvmBackend::new()?);
            boot_linux(backend, kernel, initramfs, guest_ram_len, cmdline)?
        }
    };
    let work = Box::new(crate::work_perf::PerfWorkCounter::open()?);
    let wiring = crate::vmm::VtimeWiring::new(crate::vmm::contract_vclock_config(), work, seed)?;
    vmm.wire_vtime(wiring);
    Ok(vmm)
}

#[cfg(test)]
mod tests {
    //! Mock-backed composition tests. [`compose`] runs on **every** platform (no
    //! host gate), so this is where the `GuestRam` allocation and the `unsafe`
    //! `map_memory` pointer seam get exercised under Miri (Vec-backed `GuestRam`,
    //! like `vmm-backend`'s region seam) and on the Linux box. [`boot`]'s host-gate
    //! wiring is covered separately.

    use vmm_backend::{Exit, MockBackend};

    use super::*;
    use crate::vmm::TerminalReason;

    /// Task 95 M2.2: [`compose_restore_target`] maps the materialized snapshot
    /// AS the guest RAM — the marker page reads through, no loader ran (the
    /// image is exactly the snapshot, zeros where the snapshot is zero), the
    /// xAPIC is wired on request, and the backing is the mapping itself.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "materialize/Mapping use file-backed mmap, which Miri cannot execute; the map seam \
                  is covered under Miri by compose_drives_guestram_and_unsafe_map_memory"
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

    /// 40 KiB — the minimum 4 KiB-multiple covering the boot-info struct at
    /// `BOOT_INFO_GPA = 0x9000` (`+0x78`) and the synthetic load region. Small so
    /// the Miri-interpreted `GuestRam` alloc + `state_blob` read stay quick.
    const GUEST_RAM_LEN: usize = 0xA000;
    const MB_HEADER_MAGIC: u32 = 0x1BAD_B002;
    const HDR_OFF: usize = 0x1000;
    const LOAD_ADDR: u32 = 0x2000;
    const LOAD_END: u32 = 0x2040;
    const BSS_END: u32 = 0x2080;
    const ENTRY_ADDR: u32 = 0x2000;
    const MARKER: u8 = 0x5A;

    /// Hand-build a minimal address-override Multiboot image: a valid 32-byte
    /// header (magic, address-override flag bit 16, `magic+flags+checksum == 0`, and
    /// the address fields) at file offset `HDR_OFF`, with `header_addr == load_addr`
    /// so the override formula yields `file_off = HDR_OFF`. The header sits inside
    /// the loadable region, exactly like the task-04 payloads.
    fn synthetic_multiboot() -> Vec<u8> {
        let flags: u32 = 1 << 16; // address-override (bit 16)
        let checksum = 0u32.wrapping_sub(MB_HEADER_MAGIC).wrapping_sub(flags);
        let fields = [
            MB_HEADER_MAGIC,
            flags,
            checksum,
            LOAD_ADDR, // header_addr == load_addr ⇒ file_off = HDR_OFF − 0
            LOAD_ADDR,
            LOAD_END,
            BSS_END,
            ENTRY_ADDR,
        ];
        let copy_len = (LOAD_END - LOAD_ADDR) as usize; // 0x40
        let mut img = vec![0u8; HDR_OFF + copy_len];
        for (i, f) in fields.iter().enumerate() {
            img[HDR_OFF + i * 4..HDR_OFF + i * 4 + 4].copy_from_slice(&f.to_le_bytes());
        }
        img[HDR_OFF + 32] = MARKER; // just past the 32-byte header
        img
    }

    #[test]
    fn compose_drives_guestram_and_unsafe_map_memory() {
        let payload = synthetic_multiboot();
        // A scripted clean isa-debug-exit PASS so `run()` reaches terminal.
        let backend = MockBackend::with_exits(vec![Exit::Io {
            port: 0xF4,
            size: 1,
            write: Some(0),
        }]);

        // compose(): GuestRam::new (Vec-backed under Miri) → multiboot::load →
        // write_boot_info → unsafe map_memory(.., ram.as_mut_bytes()) → restore.
        // The RAM-pointer lifetime/bounds seam Miri must check runs here.
        let mut vmm = compose(backend, &payload, GUEST_RAM_LEN).expect("compose succeeds");

        let result = vmm.run().expect("run to terminal");
        assert_eq!(result.reason, TerminalReason::DebugExit { code: 0 });

        // state_blob re-reads the owned GuestRam (the `MEM\0` chunk: `b"MEM\0" ‖
        // len(u64 LE) ‖ raw guest RAM`) — the backing outlived map_memory.
        let blob = vmm.state_blob();
        assert_eq!(&blob[0..4], b"MEM\0");
        assert!(blob.len() >= 12 + GUEST_RAM_LEN);
        let mem = &blob[12..12 + GUEST_RAM_LEN];
        // The 32-byte header was copied to guest RAM at LOAD_ADDR from file_off
        // 0x1000 (the override formula's file-offset term is honored).
        assert_eq!(
            u32::from_le_bytes(
                mem[LOAD_ADDR as usize..LOAD_ADDR as usize + 4]
                    .try_into()
                    .unwrap()
            ),
            MB_HEADER_MAGIC,
        );
        assert_eq!(mem[LOAD_ADDR as usize + 32], MARKER);
        // write_boot_info zeroed the minimal info struct at BOOT_INFO_GPA (0x9000).
        assert!(mem[0x9000..0x9078].iter().all(|&b| b == 0));
    }

    #[test]
    fn apply_entry_overlays_registers_segments_and_control_regs() {
        let entry = entry::protected_mode_entry(0x10_0000, entry::BOOT_INFO_GPA);
        let mut state = VcpuState::default();
        apply_entry(&mut state, &entry);
        // The Multiboot handoff registers/segments/control-regs are overlaid (a
        // no-op `apply_entry` would leave the default zeros).
        assert_eq!(
            state.regs.rax,
            u64::from(multiboot::MULTIBOOT_BOOTLOADER_MAGIC)
        );
        assert_eq!(state.regs.rip, 0x10_0000);
        assert_eq!(state.sregs.cs.limit, 0xFFFF_FFFF);
        assert_eq!(state.sregs.cs.selector, 0x08);
        assert_eq!(state.sregs.cr0, entry.sregs.cr0);
        assert_eq!(state.sregs.efer, entry.sregs.efer);
        assert!(matches!(state.mp_state, MpState::Runnable));
    }

    #[test]
    fn boot_runs_the_host_assert_then_composes() {
        let payload = synthetic_multiboot();
        let backend = MockBackend::with_exits(vec![]);
        // boot() runs the §1.1 host-assert first, then composes. Off the box the
        // assert is a no-op and boot composes successfully; on a non-baseline box it
        // returns HostAssert *before* composing. Either is correct — but never some
        // other error (which would mean composition broke).
        match boot(backend, &payload, GUEST_RAM_LEN) {
            Ok(_) | Err(VmmError::HostAssert(_)) => {}
            Err(e) => panic!("boot returned an unexpected error: {e}"),
        }
    }

    // --- Linux path (task 30) ---------------------------------------------

    /// Hand-build a minimal valid bzImage via direct byte writes: the gating magics
    /// (`boot_flag`/`HdrS`/`version ≥ 2.12`/`XLF_KERNEL_64`), `setup_sects = 1`,
    /// `pref_address`, realistic `cmdline_size`/`initrd_addr_max`, and `pm_len`
    /// marker bytes after `(setup_sects+1)*512`.
    fn synthetic_bzimage(pref_address: u32, pm_len: usize) -> Vec<u8> {
        let pm_off = (1 + 1) * 512usize; // setup_sects=1 ⇒ 0x400
        let mut img = vec![0u8; pm_off + pm_len];
        img[0x1f1] = 1; // setup_sects
        img[0x1fe..0x200].copy_from_slice(&0xAA55u16.to_le_bytes()); // boot_flag
        img[0x202..0x206].copy_from_slice(&0x5372_6448u32.to_le_bytes()); // "HdrS"
        img[0x206..0x208].copy_from_slice(&0x020fu16.to_le_bytes()); // version 2.15
        img[0x22c..0x230].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes()); // initrd_addr_max
        img[0x236..0x238].copy_from_slice(&1u16.to_le_bytes()); // xloadflags = XLF_KERNEL_64
        img[0x238..0x23c].copy_from_slice(&0x7FFu32.to_le_bytes()); // cmdline_size
        img[0x258..0x260].copy_from_slice(&u64::from(pref_address).to_le_bytes()); // pref_address
        for (i, b) in img[pm_off..].iter_mut().enumerate() {
            *b = (0x11 + (i % 0x40)) as u8;
        }
        img
    }

    #[test]
    fn image_kind_detects_linux_multiboot_and_garbage() {
        assert_eq!(
            ImageKind::detect(&synthetic_bzimage(0x10_0000, 0x400)),
            Some(ImageKind::Linux)
        );
        assert_eq!(
            ImageKind::detect(&synthetic_multiboot()),
            Some(ImageKind::Multiboot)
        );
        assert_eq!(ImageKind::detect(&[0u8; 4096]), None);
        assert_eq!(ImageKind::detect(&[]), None);
    }

    #[test]
    fn apply_linux_entry_overlays_long_mode_state_gdtr_and_efer_msr() {
        let entry = entry::long_mode_entry(0x10_0200, 0x7000, 0x1000, 0x6000);
        let mut state = VcpuState::default();
        apply_linux_entry(&mut state, &entry);
        // Long-mode registers/segments/control-regs overlaid.
        assert_eq!(state.regs.rip, 0x10_0200);
        assert_eq!(state.regs.rsi, 0x7000);
        assert_eq!(state.sregs.cs.selector, 0x10);
        assert_eq!(state.sregs.cs.l, 1);
        assert_eq!(state.sregs.cr3, 0x1000);
        assert_eq!(state.sregs.cr0, entry.sregs.cr0);
        assert_eq!(state.sregs.cr4, entry.sregs.cr4);
        assert_eq!(state.sregs.efer, entry.sregs.efer);
        // GDTR points at the loader's boot GDT (unlike the Multiboot overlay).
        assert_eq!(state.sregs.gdt.base, 0x6000);
        // EFER is ALSO written into the allow-stateful MSR map (else `restore`
        // clobbers it back to the reset value after SET_SREGS2 → FAIL_ENTRY).
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
        let backend = MockBackend::with_exits(vec![Exit::Hlt]);
        let ram = 0x20_0000usize; // 2 MiB (4 KiB-multiple, > pref_address + kernel)
        let mut vmm =
            compose_linux(backend, &kernel, &[], ram, "console=ttyS0").expect("compose_linux");

        // The Linux path wires the userspace xAPIC.
        assert!(vmm.lapic_wired());
        let r = vmm.run().expect("run");
        assert_eq!(r.reason, TerminalReason::Hlt);

        // The kernel was copied to pref_address and boot_params carries the HdrS
        // magic — i.e. the loader actually ran inside compose_linux.
        let blob = vmm.state_blob();
        let mem = &blob[12..12 + ram];
        assert_eq!(mem[0x10_0000], 0x11, "kernel copied to pref_address");
        assert_eq!(
            u32::from_le_bytes(mem[0x7202..0x7206].try_into().unwrap()),
            0x5372_6448,
            "boot_params.hdr.header == HdrS"
        );
    }
}
