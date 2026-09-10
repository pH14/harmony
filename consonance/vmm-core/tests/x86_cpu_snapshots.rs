// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded x86 CPU snapshot regression coverage.
//!
//! The live gate uses a deliberately small 32-bit protected-mode PAE guest. It
//! fills the hardware PDPTR cache from one page directory, emits a scalar UART
//! warmup while RAM still names that directory, then changes the guest PDPT in
//! RAM without reloading CR3 and snapshots at the resulting PIO boundary. A
//! complete save/encode/decode/restore must retain the cached page directory
//! even though the RAM image now names another one.
//!
//! The warmup makes the fixture's starting point a valid running stop. It does
//! not claim a particular KVM first-entry cache behavior; the earlier
//! pre-first-entry explanation remains an unproven hypothesis.
//!
//! The KVM test is ignored because it needs Linux x86-64 `/dev/kvm`; the small
//! mock test keeps the same page-aligned mapping and ownership seam exercised
//! on every host, including under Miri.

use vmm_backend::{Backend, CommonExit, Exit, Gpa, MockBackend, X86, X86Policy};
use vmm_core::vendor::x86::contract;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use vmm_core::vendor::x86::contract_vclock_config;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use vmm_core::vmm::Step;
use vmm_core::vmm::{GuestRam, TerminalReason, Vmm};

const PAGE_SIZE: usize = 4096;

/// The complete guest image is intentionally small enough to hash in the
/// live gate while still placing both 2 MiB PAE mappings in one RAM slot.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const RAM_LEN: usize = 4 * 1024 * 1024;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const CODE_GPA: usize = 0x1000;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const REMAPPED_CODE_GPA: usize = 0x201000;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const DATA_GPA: usize = 0x8000;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const REMAPPED_DATA_GPA: usize = 0x208000;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const GDT_GPA: usize = 0x7000;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PDPT_GPA: usize = 0x4000;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PD_A_GPA: usize = 0x5000;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PD_B_GPA: usize = 0x6000;

/// Entry 0 in the PDPT while the vCPU is configured. KVM loads this value into
/// its cached PDPTR state when `SREGS2_FLAGS_PDPTRS_VALID` is supplied.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PDPT_A_ENTRY: u64 = 0x5001;
/// Entry 0 after the host mutates the guest RAM. The cached value must remain
/// [`PDPT_A_ENTRY`] until the next architectural CR3/page-mode change.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PDPT_B_ENTRY: u64 = 0x6001;
/// `KVM_SREGS2_FLAGS_PDPTRS_VALID` from `asm/kvm.h`.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SREGS2_FLAGS_PDPTRS_VALID: u64 = 1;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const IA32_EFER: u32 = 0xC000_0080;
/// A scalar UART byte emitted before the RAM page-table mutation. It marks the
/// running PIO boundary that every positive source arm reaches exactly once.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const WARMUP_MARKER: u8 = 0xA5;
/// `mov edx, 0x3f8; mov al, WARMUP_MARKER; out dx, al`.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const WARMUP_LEN: usize = 5 + 2 + 1;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const WARMUP_RIP: usize = CODE_GPA + WARMUP_LEN;

/// A short bound for this guest after the warmup: one UART exit followed by one
/// HLT exit. The bound is test control only and never enters guest-visible state
/// or hashing.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const MAX_STEPS: usize = 8;

fn production_policy() -> X86Policy {
    X86Policy {
        cpuid: contract::cpuid_model(),
        msr_filter: contract::msr_filter_allow(),
    }
}

/// Compose a VMM after installing the production x86 policy and mapping its
/// caller-owned, pinned RAM. `configure` runs after mapping so it can obtain a
/// backend `save()` template (XSAVE/MSR shape is backend-specific) and restore
/// an entry state over that template.
fn compose<B, F>(mut guest_ram: GuestRam, mut backend: B, configure: F) -> Vmm<B>
where
    B: Backend<A = X86>,
    F: FnOnce(&mut B),
{
    backend
        .set_policy(&production_policy())
        .expect("install production x86 policy");
    // SAFETY: `guest_ram` is moved into the returned `Vmm` immediately after
    // this call. Parameter order drops the backend before RAM if configure
    // unwinds. `GuestRam` is page-aligned and pinned (mmap off Miri, and the
    // mock does not dereference the pointer under Miri); the VMM owns the
    // backing for the entire backend lifetime and no alias is created while a
    // backend run is in flight.
    unsafe {
        backend
            .map_memory(Gpa(0), guest_ram.as_mut_bytes())
            .expect("map guest RAM");
    }
    configure(&mut backend);
    Vmm::new(backend, guest_ram)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn put_u64(bytes: &mut [u8], gpa: usize, value: u64) {
    bytes[gpa..gpa + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn put_bytes(bytes: &mut [u8], gpa: usize, value: &[u8]) {
    bytes[gpa..gpa + value.len()].copy_from_slice(value);
}

/// Build the RAM image before it is handed to `Backend::map_memory`.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn guest_ram_image() -> GuestRam {
    let mut ram = GuestRam::new(RAM_LEN).expect("allocate guest RAM");
    let bytes = ram.as_mut_bytes();

    // `mov edx, 0x3f8; mov al, WARMUP_MARKER; out dx, al; mov al, [0x8000];
    // mov edx, 0x3f8; out dx, al; inc ebx; hlt` in a flat 32-bit code segment.
    // The same bytes at the remapped physical address make a page directory
    // mistake observable without introducing another exit. The UART port needs
    // the DX form because OUT's immediate form is limited to an 8-bit port
    // number.
    let program = [
        0xBA,
        0xF8,
        0x03,
        0x00,
        0x00,
        0xB0,
        WARMUP_MARKER,
        0xEE,
        0xA0,
        0x00,
        0x80,
        0x00,
        0x00,
        0xBA,
        0xF8,
        0x03,
        0x00,
        0x00,
        0xEE,
        0x43,
        0xF4,
    ];
    put_bytes(bytes, CODE_GPA, &program);
    put_bytes(bytes, REMAPPED_CODE_GPA, &program);
    bytes[DATA_GPA] = 0x42;
    bytes[REMAPPED_DATA_GPA] = 0x99;

    // PDPT entry 0 initially points to PD A. Both directory entries are
    // present, writable, and 2 MiB pages (`PS`), with PD B mapping VA 0..2 MiB
    // to physical 2..4 MiB.
    put_u64(bytes, PDPT_GPA, PDPT_A_ENTRY);
    put_u64(bytes, PD_A_GPA, 0x83);
    put_u64(bytes, PD_B_GPA, 0x20_00_83);

    // A real flat GDT backs the cached segment descriptors. The CPU uses the
    // descriptor-cache values supplied below; the table makes the setup
    // self-describing if KVM validates a selector against the guest image.
    put_u64(bytes, GDT_GPA, 0);
    put_u64(bytes, GDT_GPA + 8, 0x00CF_9B00_0000_FFFF);
    put_u64(bytes, GDT_GPA + 16, 0x00CF_9300_0000_FFFF);
    ram
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn code_segment() -> vmm_backend::Segment {
    vmm_backend::Segment {
        base: 0,
        limit: 0xFFFF_FFFF,
        selector: 0x8,
        type_: 0xB,
        present: 1,
        dpl: 0,
        db: 1,
        s: 1,
        l: 0,
        g: 1,
        avl: 0,
        unusable: 0,
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn data_segment() -> vmm_backend::Segment {
    vmm_backend::Segment {
        base: 0,
        limit: 0xFFFF_FFFF,
        selector: 0x10,
        type_: 0x3,
        present: 1,
        dpl: 0,
        db: 1,
        s: 1,
        l: 0,
        g: 1,
        avl: 0,
        unusable: 0,
    }
}

/// Overlay a 32-bit protected-mode PAE state onto a backend's valid save
/// template. The template supplies host-sized XSAVE and the exact allow-stateful
/// MSR key set required by a live KVM restore.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn install_pae_entry<B: Backend<A = X86>>(backend: &mut B) {
    let mut state = backend.save().expect("save backend entry template");
    let data = data_segment();
    state.regs.rip = CODE_GPA as u64;
    state.regs.rsp = (RAM_LEN - PAGE_SIZE) as u64;
    state.regs.rbx = 0;
    state.regs.rflags = 0x2;
    state.sregs.cs = code_segment();
    state.sregs.ds = data;
    state.sregs.es = data;
    state.sregs.fs = data;
    state.sregs.gs = data;
    state.sregs.ss = data;
    state.sregs.gdt = vmm_backend::DescriptorTable {
        base: GDT_GPA as u64,
        limit: 0x17,
    };
    state.sregs.cr0 = 0x8000_0011;
    state.sregs.cr2 = 0;
    state.sregs.cr3 = PDPT_GPA as u64;
    state.sregs.cr4 = 0x30;
    state.sregs.efer = 0;
    state.sregs.flags = SREGS2_FLAGS_PDPTRS_VALID;
    state.sregs.pdptrs = [PDPT_A_ENTRY, 0, 0, 0];
    state.msrs.insert(IA32_EFER, 0);
    state.mp_state = vmm_backend::MpState::Runnable;
    backend
        .restore(&state)
        .expect("restore 32-bit PAE entry state");
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn wire_snapshot_path<B: Backend<A = X86>>(vmm: &mut Vmm<B>) {
    vmm.wire_vtime(
        vmm_core::vmm::VtimeWiring::new_virtual_time(contract_vclock_config(), 0x0050_AE00)
            .expect("wire virtual time"),
    );
    vmm.wire_snapshot_hashing();
}

/// Change only the guest RAM's PDPT entry. This host write executes no guest
/// instruction and does not reload CR3, so the backend's cached PDPTR remains
/// the entry for PD A.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn switch_guest_pdpt_to_b<B: Backend<A = X86>>(vmm: &mut Vmm<B>) {
    let mut page = [0u8; PAGE_SIZE];
    page[..8].copy_from_slice(&PDPT_B_ENTRY.to_le_bytes());
    vmm.write_guest_pages(&[(u64::try_from(PDPT_GPA / PAGE_SIZE).unwrap(), page)])
        .expect("replace PDPT in guest RAM");
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn run_warmup<B: Backend<A = X86>>(vmm: &mut Vmm<B>) {
    // This is the canonical running PIO boundary: the first guest entry has
    // already executed the scalar warmup OUT while RAM still points at PD A.
    assert_eq!(
        vmm.step().expect("run scalar UART warmup"),
        Step::Continued,
        "the warmup UART OUT is a serviced, non-terminal PIO exit"
    );
    let live = vmm.vcpu_record().expect("read warmup PIO boundary state");
    assert_eq!(live.regs.rip, WARMUP_RIP as u64);
    assert_eq!(live.regs.rbx, 0);
    assert_eq!(vmm.serial(), &[WARMUP_MARKER]);
    assert_eq!(vmm.exit_counts().io, 1);
    assert_eq!(vmm.exit_counts().total(), 1);
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
struct Capture {
    memory: Vec<u8>,
    state: vm_state::VmState,
    encoded_state: Vec<u8>,
    effective_vns: Option<u64>,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn capture_at_warmup_boundary<B: Backend<A = X86>>(vmm: &Vmm<B>) -> Capture {
    let live = vmm
        .vcpu_record()
        .expect("read warmup PIO boundary vCPU state");
    assert_eq!(live.regs.rip, WARMUP_RIP as u64);
    assert_eq!(live.regs.rbx, 0);
    assert_eq!(vmm.serial(), &[WARMUP_MARKER]);
    assert_eq!(vmm.exit_counts().io, 1);
    assert_eq!(
        vmm.exit_counts().total(),
        1,
        "capture must retain exactly the warmup UART exit"
    );
    let captured_vns = vmm
        .effective_vns()
        .expect("capture must retain assigned virtual time");

    let before_hash = vmm.state_hash().expect("hash warmup-boundary state");
    let before_counts = vmm.exit_counts();
    let state = vmm.save_vm_state().expect("save warmup-boundary VM state");
    assert_ne!(
        state.sregs.flags & SREGS2_FLAGS_PDPTRS_VALID,
        0,
        "save must carry the SREGS2 PDPTRS_VALID flag"
    );
    assert_eq!(
        state.sregs.pdptrs[0], PDPT_A_ENTRY,
        "save must carry the cached PD A pointer, not RAM's PD B entry"
    );
    let encoded_state = state.encode().expect("encode warmup-boundary VM state");
    let decoded = vm_state::VmState::decode(&encoded_state).expect("decode VM state");
    assert_eq!(decoded, state, "VM state must round-trip through its codec");
    assert_ne!(
        decoded.sregs.flags & SREGS2_FLAGS_PDPTRS_VALID,
        0,
        "decoded state must retain SREGS2 PDPTRS_VALID"
    );
    assert_eq!(decoded.sregs.pdptrs[0], PDPT_A_ENTRY);
    assert_eq!(
        vmm.vcpu_record().unwrap(),
        live,
        "capture executes no instruction"
    );
    assert_eq!(vmm.exit_counts(), before_counts);
    assert_eq!(vmm.state_hash().unwrap(), before_hash);
    assert_eq!(vmm.effective_vns(), Some(captured_vns));
    assert_eq!(state.vtime.snapshot_vns, captured_vns);
    Capture {
        memory: vmm.guest_memory().to_vec(),
        state: decoded,
        encoded_state,
        effective_vns: Some(captured_vns),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
struct Endpoint {
    serial: Vec<u8>,
    memory: Vec<u8>,
    encoded_state: Vec<u8>,
    state_blob: Vec<u8>,
    state_hash: [u8; 32],
    effective_vns: Option<u64>,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn run_bounded<B: Backend<A = X86>>(
    vmm: &mut Vmm<B>,
    expected_byte: u8,
    expected_pdptr: u64,
) -> Endpoint {
    let mut steps = 0;
    loop {
        assert!(steps < MAX_STEPS, "guest exceeded the bounded step budget");
        match vmm.step().expect("run one guest exit") {
            Step::Continued => steps += 1,
            Step::Terminal(reason) => {
                assert_eq!(reason, TerminalReason::Idle, "guest stopped at HLT");
                break;
            }
            Step::SdkStop => panic!("unexpected SDK stop in the CPU snapshot guest"),
        }
    }
    assert_eq!(
        vmm.serial(),
        &[WARMUP_MARKER, expected_byte],
        "the cached page directory must select the expected data byte after warmup"
    );
    let state = vmm.save_vm_state().expect("save endpoint VM state");
    assert_eq!(state.regs.rbx, 1, "guest INC EBX must retire exactly once");
    assert_eq!(state.sregs.pdptrs[0], expected_pdptr);
    Endpoint {
        serial: vmm.serial().to_vec(),
        memory: vmm.guest_memory().to_vec(),
        encoded_state: state.encode().expect("encode endpoint VM state"),
        state_blob: vmm.state_blob().expect("encode endpoint state blob"),
        state_hash: vmm.state_hash().expect("hash endpoint state"),
        effective_vns: vmm.effective_vns(),
    }
}

#[test]
fn compose_maps_guest_ram_and_keeps_it_alive_for_mock_backend() {
    let mut vmm = compose(
        GuestRam::new(PAGE_SIZE).expect("allocate mock RAM"),
        MockBackend::with_exits([Exit::Common(CommonExit::Idle)]),
        |_| {},
    );
    assert_eq!(vmm.guest_memory().len(), PAGE_SIZE);
    assert_eq!(
        vmm.run().expect("run mapped mock guest").reason,
        TerminalReason::Idle
    );
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]
mod live_kvm {
    use super::*;
    use vmm_backend::KvmBackend;

    fn require_kvm() {
        assert!(
            std::path::Path::new("/dev/kvm").exists(),
            "/dev/kvm missing — run this ignored live gate on a Linux x86-64 KVM host"
        );
    }

    fn fresh_vmm(configure: bool) -> Vmm<KvmBackend> {
        let backend = KvmBackend::new().unwrap_or_else(|e| panic!("KvmBackend::new failed: {e}"));
        let configure: fn(&mut KvmBackend) = if configure {
            install_pae_entry::<KvmBackend>
        } else {
            |_| {}
        };
        compose(guest_ram_image(), backend, configure)
    }

    #[test]
    #[ignore = "live KVM; run with --ignored on a Linux x86-64 /dev/kvm host"]
    fn pae_cached_pdptrs_survive_full_vmm_snapshot_restore() {
        require_kvm();

        // Uninterrupted reference: configure with PD A and execute the exact
        // warmup that establishes the running PIO boundary before mutating RAM
        // to PD B. No snapshot operation intervenes.
        let mut uninterrupted = fresh_vmm(true);
        wire_snapshot_path(&mut uninterrupted);
        run_warmup(&mut uninterrupted);
        switch_guest_pdpt_to_b(&mut uninterrupted);
        let saved_at_warmup = uninterrupted.vcpu_record().expect("save cached PDPTRs");
        assert_ne!(
            saved_at_warmup.sregs.flags & SREGS2_FLAGS_PDPTRS_VALID,
            0,
            "KVM save must expose PDPTRS_VALID"
        );
        assert_eq!(saved_at_warmup.sregs.pdptrs[0], PDPT_A_ENTRY);
        let uninterrupted_endpoint = run_bounded(&mut uninterrupted, 0x42, PDPT_A_ENTRY);

        // Save-and-continue: execute the same warmup, mutate RAM, and capture at
        // the same running PIO boundary. This proves save/encode/decode itself
        // does not advance time, retire an instruction, or rewrite the cache.
        let mut save_and_continue = fresh_vmm(true);
        wire_snapshot_path(&mut save_and_continue);
        run_warmup(&mut save_and_continue);
        switch_guest_pdpt_to_b(&mut save_and_continue);
        let capture = capture_at_warmup_boundary(&save_and_continue);
        assert_eq!(
            capture.memory[PDPT_GPA..PDPT_GPA + 8],
            PDPT_B_ENTRY.to_le_bytes()
        );
        let save_and_continue_endpoint = run_bounded(&mut save_and_continue, 0x42, PDPT_A_ENTRY);

        // Fresh restore: the target starts with RAM's original PD A entry and a
        // reset vCPU. restore_snapshot first restores the cached A PDPTRs, then
        // copies the captured RAM image whose PDPT entry is B.
        let mut restored = fresh_vmm(false);
        wire_snapshot_path(&mut restored);
        restored
            .restore_snapshot(&capture.memory, &capture.state)
            .expect("restore full VM snapshot into fresh KVM VMM");
        assert_eq!(restored.effective_vns(), capture.effective_vns);
        assert_eq!(
            restored.save_vm_state().unwrap().encode().unwrap(),
            capture.encoded_state
        );
        let restored_endpoint = run_bounded(&mut restored, 0x42, PDPT_A_ENTRY);

        assert_eq!(
            uninterrupted_endpoint, save_and_continue_endpoint,
            "uninterrupted and save-and-continue endpoints must be byte-identical"
        );
        assert_eq!(
            save_and_continue_endpoint, restored_endpoint,
            "fresh restore must reproduce CPU, RAM, VMM bytes, hash, UART, and V-time"
        );

        // Negative control: with the saved validity bit and cached pointers
        // explicitly removed, KVM must consult RAM's PD B and expose 0x99. If
        // this fails on a host, retain the failure as evidence about that KVM
        // implementation; the positive regression must not be weakened.
        let mut no_cached_pdptrs = capture.state.clone();
        no_cached_pdptrs.sregs.flags &= !SREGS2_FLAGS_PDPTRS_VALID;
        no_cached_pdptrs.sregs.pdptrs = [0; 4];
        let mut negative = fresh_vmm(false);
        wire_snapshot_path(&mut negative);
        // `restore_snapshot` restores CPU state before copying the RAM image.
        // Seed the target RAM with PD B so a restore without explicit PDPTRs
        // makes KVM load B from memory during its `SET_SREGS2` operation.
        switch_guest_pdpt_to_b(&mut negative);
        negative
            .restore_snapshot(&capture.memory, &no_cached_pdptrs)
            .expect("restore negative control without cached PDPTRs");
        let negative_endpoint = run_bounded(&mut negative, 0x99, PDPT_B_ENTRY);
        assert_eq!(
            negative_endpoint.serial,
            vec![WARMUP_MARKER, 0x99],
            "without PDPTRS_VALID, RAM's PD B must select the remapped data"
        );
        println!(
            "PAE snapshot witness: all three continuations read 0x42; omitted PDPTR cache reads 0x99"
        );
    }
}
