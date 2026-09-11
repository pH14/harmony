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
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const GUEST_PDPT_WRITE_LEN: usize = 10;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const BASE_PROGRAM_LEN: usize = 21;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const GUEST_PDPT_WRITE: [u8; GUEST_PDPT_WRITE_LEN] =
    [0xC7, 0x05, 0x00, 0x40, 0x00, 0x00, 0x01, 0x60, 0x00, 0x00];

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
/// Prefix both physical copies of the guest program with a guest-authored
/// PDPT write. The original 21-byte program remains unchanged after the
/// prefix, so the only added behavior is the guest RAM mutation.
fn guest_ram_image_with_guest_pdpt_write() -> GuestRam {
    let mut ram = guest_ram_image();
    let bytes = ram.as_mut_bytes();
    bytes.copy_within(
        CODE_GPA..CODE_GPA + BASE_PROGRAM_LEN,
        CODE_GPA + GUEST_PDPT_WRITE_LEN,
    );
    bytes.copy_within(
        REMAPPED_CODE_GPA..REMAPPED_CODE_GPA + BASE_PROGRAM_LEN,
        REMAPPED_CODE_GPA + GUEST_PDPT_WRITE_LEN,
    );
    put_bytes(bytes, CODE_GPA, &GUEST_PDPT_WRITE);
    put_bytes(bytes, REMAPPED_CODE_GPA, &GUEST_PDPT_WRITE);
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
    use vmm_core::virtual_time::{DeviceClass, NormalizedEventClass};

    const MMIO_RAM_LEN: usize = 64 * 1024;
    const MMIO_CODE_GPA: usize = 0x1000;
    const MMIO_TPR_GPA: u64 = 0xFEE0_0080;
    const MMIO_TPR_VALUE: u32 = 5;
    /// `ADD dword ptr [0xFEE0_0080], 5; INC BX; HLT`.
    ///
    /// The address-size and operand-size prefixes make the first instruction
    /// nine bytes long while the final `INC BX` remains the one-byte real-mode
    /// witness. KVM exposes the xAPIC read and write as two userspace exits.
    const MMIO_PROGRAM: [u8; 11] = [
        0x67,
        0x66,
        0x83,
        0x05,
        MMIO_TPR_GPA as u8,
        (MMIO_TPR_GPA >> 8) as u8,
        (MMIO_TPR_GPA >> 16) as u8,
        (MMIO_TPR_GPA >> 24) as u8,
        MMIO_TPR_VALUE as u8,
        0x43,
        0xF4,
    ];

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

    fn fresh_guest_pdpt_vmm(configure: bool) -> Vmm<KvmBackend> {
        let backend = KvmBackend::new().unwrap_or_else(|e| panic!("KvmBackend::new failed: {e}"));
        let configure: fn(&mut KvmBackend) = if configure {
            install_pae_entry::<KvmBackend>
        } else {
            |_| {}
        };
        compose(guest_ram_image_with_guest_pdpt_write(), backend, configure)
    }

    fn mmio_guest_ram() -> GuestRam {
        let mut ram = GuestRam::new(MMIO_RAM_LEN).expect("allocate MMIO guest RAM");
        ram.as_mut_bytes()[MMIO_CODE_GPA..MMIO_CODE_GPA + MMIO_PROGRAM.len()]
            .copy_from_slice(&MMIO_PROGRAM);
        ram
    }

    /// Overlay a flat real/unreal-mode entry state onto KVM's valid save
    /// template. The 32-bit address-size prefix still uses the data segment's
    /// hidden limit, so the maximal DS limit is what lets the instruction reach
    /// the xAPIC page above the 64 KiB RAM image.
    fn install_mmio_entry<B: Backend<A = X86>>(backend: &mut B) {
        let mut state = backend.save().expect("save MMIO entry template");
        state.sregs.cs.base = 0;
        state.sregs.cs.selector = 0;
        state.sregs.ds.base = 0;
        state.sregs.ds.selector = 0;
        state.sregs.ds.limit = u32::MAX;
        // VM entry requires page granularity for this 4 GiB effective limit.
        state.sregs.ds.g = 1;
        state.regs.rip = MMIO_CODE_GPA as u64;
        state.regs.rflags = 0x2;
        state.regs.rbx = 0;
        state.mp_state = vmm_backend::MpState::Runnable;
        backend.restore(&state).expect("restore MMIO entry state");
    }

    fn fresh_mmio_vmm() -> Vmm<KvmBackend> {
        let backend = KvmBackend::new().unwrap_or_else(|e| panic!("KvmBackend::new failed: {e}"));
        let mut vmm = compose(mmio_guest_ram(), backend, install_mmio_entry::<KvmBackend>);
        wire_snapshot_path(&mut vmm);
        vmm.wire_lapic(
            lapic::Lapic::new(lapic::LapicConfig {
                apic_id: 0,
                timer_hz: 24_000_000,
            })
            .expect("wire 24 MHz LAPIC"),
        );
        vmm
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct MmioCapture {
        memory: Vec<u8>,
        state: vm_state::VmState,
        encoded_state: Vec<u8>,
        state_blob: Vec<u8>,
        state_hash: [u8; 32],
        moment: Option<u64>,
    }

    fn capture_full_vmm(vmm: &Vmm<KvmBackend>) -> MmioCapture {
        let state = vmm.save_vm_state().expect("save full MMIO VM state");
        let encoded_state = state.encode().expect("encode full MMIO VM state");
        let decoded = vm_state::VmState::decode(&encoded_state).expect("decode full MMIO VM state");
        assert_eq!(decoded, state, "full MMIO VM state codec must round-trip");
        MmioCapture {
            memory: vmm.guest_memory().to_vec(),
            state: decoded,
            encoded_state,
            state_blob: vmm.state_blob().expect("encode full MMIO state blob"),
            state_hash: vmm.state_hash().expect("hash full MMIO VM state"),
            moment: vmm.effective_vns(),
        }
    }

    fn report_root(variable: &str) -> Option<std::path::PathBuf> {
        std::env::var_os(variable).map(std::path::PathBuf::from)
    }

    /// Retain raw phase bytes before a later cross-arm assertion can discard
    /// the useful failure evidence. Reports are opt-in so the regression stays
    /// portable; the hosted KVM workflow supplies the directory.
    fn retain_capture(
        root: Option<&std::path::Path>,
        label: &str,
        capture: &MmioCapture,
        guest_bytes: &[u8],
    ) {
        let Some(root) = root else { return };
        let directory = root.join(label);
        std::fs::create_dir_all(&directory).expect("create snapshot report directory");
        std::fs::write(directory.join("memory.bin"), &capture.memory)
            .expect("write snapshot memory report");
        std::fs::write(directory.join("vm-state.bin"), &capture.encoded_state)
            .expect("write snapshot state report");
        std::fs::write(directory.join("state-blob.bin"), &capture.state_blob)
            .expect("write snapshot state-blob report");
        std::fs::write(directory.join("guest-bytes.bin"), guest_bytes)
            .expect("write guest-byte report");
        let summary = format!(
            "state_hash={:?}\nvirtual_time={:?}\nrip={}\nrbx={}\n",
            capture.state_hash, capture.moment, capture.state.regs.rip, capture.state.regs.rbx,
        );
        std::fs::write(directory.join("summary.txt"), summary)
            .expect("write snapshot summary report");
    }

    fn retain_endpoint(
        root: Option<&std::path::Path>,
        label: &str,
        endpoint: &Endpoint,
        guest_bytes: &[u8],
    ) {
        let Some(root) = root else { return };
        let directory = root.join(label);
        std::fs::create_dir_all(&directory).expect("create endpoint report directory");
        std::fs::write(directory.join("memory.bin"), &endpoint.memory)
            .expect("write endpoint memory report");
        std::fs::write(directory.join("vm-state.bin"), &endpoint.encoded_state)
            .expect("write endpoint state report");
        std::fs::write(directory.join("state-blob.bin"), &endpoint.state_blob)
            .expect("write endpoint state-blob report");
        std::fs::write(directory.join("guest-bytes.bin"), guest_bytes)
            .expect("write endpoint guest-byte report");
        let summary = format!(
            "serial={:?}\nstate_hash={:?}\nvirtual_time={:?}\n",
            endpoint.serial, endpoint.state_hash, endpoint.effective_vns,
        );
        std::fs::write(directory.join("summary.txt"), summary)
            .expect("write endpoint summary report");
    }

    /// Read the LAPIC TPR from the opaque x86 device record after the public
    /// `VmState` codec has decoded it. This keeps the test independent of a
    /// product-only inspection accessor while proving the RMW's device result
    /// itself was saved and restored.
    fn lapic_tpr(state: &vm_state::VmState) -> u32 {
        let bytes = &state.devices.0;
        let read_u32 = |offset: usize| {
            let end = offset.checked_add(4).expect("device offset overflow");
            let field = bytes
                .get(offset..end)
                .expect("truncated LAPIC device record");
            u32::from_le_bytes(field.try_into().expect("LAPIC field width"))
        };
        let report_len = usize::try_from(read_u32(14)).expect("report length fits usize");
        let capture_len_offset = 18usize
            .checked_add(report_len.checked_mul(4).expect("report length overflow"))
            .expect("capture length offset overflow");
        let capture_len =
            usize::try_from(read_u32(capture_len_offset)).expect("capture length fits usize");
        let lapic_flag = capture_len_offset
            .checked_add(4)
            .and_then(|offset| offset.checked_add(capture_len))
            .and_then(|offset| offset.checked_add(10))
            .expect("LAPIC flag offset overflow");
        assert_eq!(
            *bytes.get(lapic_flag).expect("truncated LAPIC flag"),
            1,
            "the saved MMIO fixture must carry a LAPIC"
        );
        read_u32(lapic_flag.checked_add(1 + 16).expect("TPR offset overflow"))
    }

    fn capture_mmio_boundary(vmm: &Vmm<KvmBackend>, before_vns: u64) -> MmioCapture {
        let live = vmm.vcpu_record().expect("read MMIO boundary vCPU");
        assert_eq!(live.regs.rip, (MMIO_CODE_GPA + 9) as u64);
        assert_eq!(
            live.regs.rbx, 0,
            "INC BX must remain unretired at the boundary"
        );

        let counts = vmm.exit_counts();
        assert_eq!(
            counts.mmio, 2,
            "one VMM step must service the LAPIC read and write"
        );
        assert_eq!(
            counts.total(),
            2,
            "the MMIO instruction has exactly two exits"
        );

        let trace = vmm
            .virtual_time_trace()
            .expect("MMIO fixture wires the production virtual-time trace");
        assert_eq!(
            trace.raw_log().len(),
            2,
            "the trace has two raw MMIO exits before HLT"
        );
        assert_eq!(
            trace.normalized_log().events.len(),
            2,
            "the trace has two MMIO events before HLT"
        );
        for event in &trace.normalized_log().events {
            assert_eq!(
                event.class,
                NormalizedEventClass::DeviceMmio(DeviceClass::InterruptController)
            );
        }
        let cost = contract::virtual_time_timing().interrupt_controller_mmio_vns;
        assert_eq!(
            trace.normalized_log().events[0].vns_after,
            before_vns + cost
        );
        assert_eq!(
            trace.normalized_log().events[1].vns_after,
            before_vns + 2 * cost
        );

        let capture = capture_full_vmm(vmm);
        assert_eq!(
            lapic_tpr(&capture.state),
            MMIO_TPR_VALUE,
            "the ADD read/modify/write must save LAPIC TPR = 5"
        );
        assert_eq!(capture.moment, Some(before_vns + 2 * cost));
        capture
    }

    fn continue_to_hlt(vmm: &mut Vmm<KvmBackend>) -> MmioCapture {
        assert_eq!(
            vmm.step().expect("run INC BX and HLT"),
            Step::Terminal(TerminalReason::Idle),
            "the successor step must stop at HLT"
        );
        let endpoint = capture_full_vmm(vmm);
        let live = vmm.vcpu_record().expect("read MMIO endpoint vCPU");
        assert_eq!(live.regs.rbx, 1, "INC BX must retire exactly once");
        endpoint
    }

    #[test]
    #[ignore = "live KVM; run with --ignored on a Linux x86-64 /dev/kvm host"]
    fn mmio_rmw_finishes_before_full_vmm_snapshot() {
        require_kvm();

        // The uninterrupted run establishes the reference stop and endpoint.
        // The single first step must drain the ADD's LAPIC read and write before
        // exposing the boundary; the INC witness must still be pending there.
        let mut uninterrupted = fresh_mmio_vmm();
        let before_vns = uninterrupted
            .effective_vns()
            .expect("MMIO fixture wires virtual time");
        assert_eq!(
            uninterrupted.step().expect("service ADD MMIO"),
            Step::Continued
        );
        let uninterrupted_stop = capture_mmio_boundary(&uninterrupted, before_vns);
        let uninterrupted_endpoint = continue_to_hlt(&mut uninterrupted);

        // A save-and-continue source must have the same complete state at the
        // same serviced-instruction boundary. Repeated snapshot/capture calls
        // are a fixpoint: they do not execute the successor or add exits.
        let mut save_and_continue = fresh_mmio_vmm();
        let save_before_vns = save_and_continue
            .effective_vns()
            .expect("save/continue fixture wires virtual time");
        assert_eq!(save_before_vns, before_vns);
        assert_eq!(
            save_and_continue.step().expect("service saved ADD MMIO"),
            Step::Continued
        );
        let save_stop = capture_mmio_boundary(&save_and_continue, save_before_vns);
        let counts = save_and_continue.exit_counts();
        let time = save_and_continue.effective_vns();
        let bx = save_and_continue
            .vcpu_record()
            .expect("read saved boundary vCPU")
            .regs
            .rbx;
        let repeated = capture_full_vmm(&save_and_continue);
        assert!(
            repeated == save_stop,
            "repeated full snapshot is unchanged; retained MMIO stop evidence differs"
        );
        assert_eq!(save_and_continue.exit_counts(), counts);
        assert_eq!(save_and_continue.effective_vns(), time);
        assert_eq!(
            save_and_continue
                .vcpu_record()
                .expect("read repeated boundary vCPU")
                .regs
                .rbx,
            bx
        );
        assert_eq!(bx, 0, "repeated capture must not retire INC BX");
        let save_and_continue_endpoint = continue_to_hlt(&mut save_and_continue);

        // Restore the complete saved state into a fresh production-composed
        // VMM. The fresh target has no prior exits, but its restored state and
        // continuation must match the two source paths byte for byte.
        let mut cold = fresh_mmio_vmm();
        cold.restore_snapshot(&save_stop.memory, &save_stop.state)
            .expect("restore full MMIO VM snapshot");
        let cold_stop = capture_full_vmm(&cold);
        let cold_live = cold.vcpu_record().expect("read cold MMIO boundary vCPU");
        assert_eq!(cold_live.regs.rip, (MMIO_CODE_GPA + 9) as u64);
        assert_eq!(cold_live.regs.rbx, 0);
        assert_eq!(
            lapic_tpr(&cold_stop.state),
            MMIO_TPR_VALUE,
            "cold restore must retain LAPIC TPR = 5"
        );
        assert_eq!(
            cold_stop, save_stop,
            "cold restore reproduces the MMIO stop"
        );
        let cold_endpoint = continue_to_hlt(&mut cold);

        assert!(
            uninterrupted_stop == save_stop,
            "uninterrupted and saved MMIO stops differ; retained MMIO evidence is available"
        );
        assert!(
            uninterrupted_stop == cold_stop,
            "cold MMIO stop differs from the saved stop; retained MMIO evidence is available"
        );
        assert!(
            uninterrupted_endpoint == save_and_continue_endpoint,
            "uninterrupted and save-and-continue MMIO endpoints differ; retained evidence is available"
        );
        assert!(
            save_and_continue_endpoint == cold_endpoint,
            "cold restore MMIO endpoint differs from the source; retained evidence is available"
        );

        // The final instruction retired only after the stop was captured.
        assert_eq!(
            uninterrupted
                .vcpu_record()
                .expect("read uninterrupted endpoint vCPU")
                .regs
                .rbx,
            1
        );
        assert_eq!(
            save_and_continue
                .vcpu_record()
                .expect("read save/continue endpoint vCPU")
                .regs
                .rbx,
            1
        );
        assert_eq!(
            cold.vcpu_record()
                .expect("read cold endpoint vCPU")
                .regs
                .rbx,
            1
        );
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

    fn run_guest_pdpt_warmup(vmm: &mut Vmm<KvmBackend>) {
        assert_eq!(
            vmm.step().expect("run guest-authored PDPT warmup"),
            Step::Continued
        );
        assert_eq!(vmm.serial(), &[WARMUP_MARKER]);
        assert_eq!(vmm.exit_counts().io, 1);
        assert_eq!(vmm.exit_counts().total(), 1);
        assert!(
            vmm.effective_vns().is_some(),
            "guest-authored warmup must assign virtual time"
        );
    }

    fn run_guest_pdpt_endpoint(vmm: &mut Vmm<KvmBackend>) -> Endpoint {
        let mut steps = 0;
        loop {
            assert!(
                steps < MAX_STEPS,
                "guest-authored PDPT guest exceeded bound"
            );
            match vmm.step().expect("run guest-authored PDPT guest") {
                Step::Continued => steps += 1,
                Step::Terminal(reason) => {
                    assert_eq!(reason, TerminalReason::Idle);
                    break;
                }
                Step::SdkStop => panic!("unexpected SDK stop in guest-authored PDPT guest"),
            }
        }
        let serial = vmm.serial().to_vec();
        assert_eq!(serial.len(), 2);
        assert_eq!(serial[0], WARMUP_MARKER);
        assert!(matches!(serial[1], 0x42 | 0x99));
        let state = vmm.save_vm_state().expect("save guest-authored endpoint");
        assert_eq!(state.regs.rbx, 1);
        let encoded_state = state.encode().expect("encode guest-authored endpoint");
        Endpoint {
            serial,
            memory: vmm.guest_memory().to_vec(),
            encoded_state,
            state_blob: vmm
                .state_blob()
                .expect("encode guest-authored endpoint state blob"),
            state_hash: vmm.state_hash().expect("hash guest-authored endpoint"),
            effective_vns: vmm.effective_vns(),
        }
    }

    #[test]
    #[ignore = "live AMD KVM NPT observation; run with --ignored and NPT_REPORT_DIR"]
    fn amd_default_npt_pae_guest_write_observations() {
        require_kvm();
        let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").expect("read /proc/cpuinfo");
        assert!(
            cpuinfo.lines().any(|line| {
                line.split_once(':').is_some_and(|(key, value)| {
                    key.trim() == "vendor_id" && value.trim() == "AuthenticAMD"
                })
            }),
            "guest-authored observation requires an AuthenticAMD host"
        );
        let npt = std::fs::read_to_string("/sys/module/kvm_amd/parameters/npt")
            .expect("read kvm_amd NPT setting");
        assert!(matches!(npt.trim(), "Y" | "1"), "NPT must be enabled");
        let report = report_root("NPT_REPORT_DIR");

        let mut uninterrupted = fresh_guest_pdpt_vmm(true);
        wire_snapshot_path(&mut uninterrupted);
        run_guest_pdpt_warmup(&mut uninterrupted);
        let uninterrupted_endpoint = run_guest_pdpt_endpoint(&mut uninterrupted);
        retain_endpoint(
            report.as_deref(),
            "original-endpoint",
            &uninterrupted_endpoint,
            &uninterrupted_endpoint.memory[PDPT_GPA..PDPT_GPA + PAGE_SIZE],
        );

        let mut save_and_continue = fresh_guest_pdpt_vmm(true);
        wire_snapshot_path(&mut save_and_continue);
        run_guest_pdpt_warmup(&mut save_and_continue);
        let save_before_memory = save_and_continue.guest_memory().to_vec();
        let save_before_serial = save_and_continue.serial().to_vec();
        let save_before_counts = save_and_continue.exit_counts();
        let save_before_vns = save_and_continue.effective_vns();
        let save_stop = capture_full_vmm(&save_and_continue);
        retain_capture(
            report.as_deref(),
            "save-and-continue-stop",
            &save_stop,
            &save_stop.memory[PDPT_GPA..PDPT_GPA + PAGE_SIZE],
        );
        assert_eq!(
            save_and_continue.guest_memory(),
            save_before_memory.as_slice()
        );
        assert_eq!(save_and_continue.serial(), save_before_serial.as_slice());
        assert_eq!(save_and_continue.exit_counts(), save_before_counts);
        assert_eq!(save_and_continue.effective_vns(), save_before_vns);
        let save_repeated = capture_full_vmm(&save_and_continue);
        retain_capture(
            report.as_deref(),
            "save-and-continue-stop-repeat",
            &save_repeated,
            &save_repeated.memory[PDPT_GPA..PDPT_GPA + PAGE_SIZE],
        );
        assert!(
            save_stop == save_repeated,
            "guest-authored saved stop differs on repeat; retained PAE evidence is available"
        );
        assert_eq!(save_stop.state.regs.rbx, 0);
        assert_eq!(
            save_stop.state.regs.rip,
            (WARMUP_RIP + GUEST_PDPT_WRITE_LEN) as u64
        );
        assert_eq!(
            save_stop.memory[PDPT_GPA..PDPT_GPA + 8],
            PDPT_B_ENTRY.to_le_bytes()
        );
        let save_and_continue_endpoint = run_guest_pdpt_endpoint(&mut save_and_continue);
        retain_endpoint(
            report.as_deref(),
            "save-and-continue-endpoint",
            &save_and_continue_endpoint,
            &save_and_continue_endpoint.memory[PDPT_GPA..PDPT_GPA + PAGE_SIZE],
        );

        let mut cold_unobserved = fresh_guest_pdpt_vmm(false);
        wire_snapshot_path(&mut cold_unobserved);
        cold_unobserved
            .restore_snapshot(&save_stop.memory, &save_stop.state)
            .expect("restore guest-authored stop");
        let cold_unobserved_endpoint = run_guest_pdpt_endpoint(&mut cold_unobserved);
        retain_endpoint(
            report.as_deref(),
            "cold-unobserved-endpoint",
            &cold_unobserved_endpoint,
            &cold_unobserved_endpoint.memory[PDPT_GPA..PDPT_GPA + PAGE_SIZE],
        );

        let mut cold_observed = fresh_guest_pdpt_vmm(false);
        wire_snapshot_path(&mut cold_observed);
        cold_observed
            .restore_snapshot(&save_stop.memory, &save_stop.state)
            .expect("restore observed guest-authored stop");
        let cold_before_memory = cold_observed.guest_memory().to_vec();
        let cold_before_serial = cold_observed.serial().to_vec();
        let cold_before_counts = cold_observed.exit_counts();
        let cold_before_vns = cold_observed.effective_vns();
        let cold_stop = capture_full_vmm(&cold_observed);
        retain_capture(
            report.as_deref(),
            "cold-observed-stop",
            &cold_stop,
            &cold_stop.memory[PDPT_GPA..PDPT_GPA + PAGE_SIZE],
        );
        assert_eq!(cold_observed.guest_memory(), cold_before_memory.as_slice());
        assert_eq!(cold_observed.serial(), cold_before_serial.as_slice());
        assert_eq!(cold_observed.exit_counts(), cold_before_counts);
        assert_eq!(cold_observed.effective_vns(), cold_before_vns);
        let cold_repeated = capture_full_vmm(&cold_observed);
        retain_capture(
            report.as_deref(),
            "cold-observed-stop-repeat",
            &cold_repeated,
            &cold_repeated.memory[PDPT_GPA..PDPT_GPA + PAGE_SIZE],
        );
        assert!(
            cold_stop == cold_repeated,
            "guest-authored cold stop differs on repeat; retained PAE evidence is available"
        );
        assert!(
            cold_stop == save_stop,
            "guest-authored cold stop differs from saved stop; retained PAE evidence is available"
        );
        let cold_observed_endpoint = run_guest_pdpt_endpoint(&mut cold_observed);
        retain_endpoint(
            report.as_deref(),
            "cold-observed-endpoint",
            &cold_observed_endpoint,
            &cold_observed_endpoint.memory[PDPT_GPA..PDPT_GPA + PAGE_SIZE],
        );

        assert!(
            uninterrupted_endpoint == save_and_continue_endpoint,
            "guest-authored original and save-and-continue futures differ; retained PAE evidence is available"
        );
        assert!(
            save_and_continue_endpoint == cold_unobserved_endpoint,
            "guest-authored cold continuation differs; retained PAE evidence is available"
        );
        assert!(
            cold_unobserved_endpoint == cold_observed_endpoint,
            "observing the cold stop changes its future; retained PAE evidence is available"
        );
        println!(
            "AMD NPT guest-authored PAE observation: serial={:?} state_hash={:?}",
            uninterrupted_endpoint.serial, uninterrupted_endpoint.state_hash
        );
    }

    const XSAVE_GPA: usize = 0x9000;
    const XSAVE_PAGE_LEN: usize = PAGE_SIZE;
    const XSAVE_PROGRAM_LEN: usize = 34;
    const XSAVE_ENDPOINT_RIP: usize = CODE_GPA + XSAVE_PROGRAM_LEN;
    const XSAVE_MARKER: u8 = 0x42;
    /// `FNINIT; UART A5; EAX=3, EDX=0; XSAVE [0x9000]; UART 42; INC EBX; HLT`.
    const XSAVE_PROGRAM: [u8; XSAVE_PROGRAM_LEN] = [
        0xDB,
        0xE3, // FNINIT
        0xBA,
        0xF8,
        0x03,
        0x00,
        0x00, // MOV EDX, 0x3f8
        0xB0,
        WARMUP_MARKER,
        0xEE, // OUT DX, AL
        0xB8,
        0x03,
        0x00,
        0x00,
        0x00, // MOV EAX, 3
        0x31,
        0xD2, // XOR EDX, EDX
        0x0F,
        0xAE,
        0x25,
        0x00,
        0x90,
        0x00,
        0x00, // XSAVE [0x9000]
        0xBA,
        0xF8,
        0x03,
        0x00,
        0x00, // MOV EDX, 0x3f8
        0xB0,
        XSAVE_MARKER,
        0xEE,
        0x43,
        0xF4,
    ];

    fn xsave_guest_ram_image() -> GuestRam {
        let mut ram = guest_ram_image();
        let bytes = ram.as_mut_bytes();
        bytes[XSAVE_GPA..XSAVE_GPA + XSAVE_PAGE_LEN].fill(0);
        put_bytes(bytes, CODE_GPA, &XSAVE_PROGRAM);
        put_bytes(bytes, REMAPPED_CODE_GPA, &XSAVE_PROGRAM);
        ram
    }

    fn install_xsave_entry(backend: &mut KvmBackend) {
        install_pae_entry(backend);
        let mut state = backend.save().expect("save XSAVE entry template");
        state.sregs.cr4 |= (1 << 9) | (1 << 18); // OSFXSR | OSXSAVE
        state.sregs.cr0 &= !((1 << 2) | (1 << 3)); // EM | TS clear
        state.xcr0 = 3; // x87 + SSE
        backend
            .restore(&state)
            .expect("restore XSAVE-capable entry state");
    }

    fn fresh_xsave_vmm(configure: bool) -> Vmm<KvmBackend> {
        let backend = KvmBackend::new().unwrap_or_else(|e| panic!("KvmBackend::new failed: {e}"));
        let configure: fn(&mut KvmBackend) = if configure {
            install_xsave_entry
        } else {
            |_| {}
        };
        compose(xsave_guest_ram_image(), backend, configure)
    }

    fn xsave_output_page(memory: &[u8]) -> &[u8] {
        memory
            .get(XSAVE_GPA..XSAVE_GPA + XSAVE_PAGE_LEN)
            .expect("XSAVE output page is within guest RAM")
    }

    fn run_xsave_warmup(vmm: &mut Vmm<KvmBackend>) {
        assert_eq!(vmm.step().expect("run XSAVE warmup"), Step::Continued);
        assert_eq!(vmm.serial(), &[WARMUP_MARKER]);
        assert_eq!(vmm.exit_counts().io, 1);
        assert_eq!(vmm.exit_counts().total(), 1);
        assert!(
            vmm.effective_vns().is_some(),
            "XSAVE warmup must assign virtual time"
        );
    }

    fn run_xsave_endpoint(vmm: &mut Vmm<KvmBackend>) -> MmioCapture {
        let mut steps = 0;
        loop {
            assert!(steps < MAX_STEPS, "XSAVE guest exceeded bound");
            match vmm.step().expect("run XSAVE guest") {
                Step::Continued => steps += 1,
                Step::Terminal(reason) => {
                    assert_eq!(reason, TerminalReason::Idle);
                    break;
                }
                Step::SdkStop => panic!("unexpected SDK stop in XSAVE guest"),
            }
        }
        assert_eq!(vmm.serial(), &[WARMUP_MARKER, XSAVE_MARKER]);
        let capture = capture_full_vmm(vmm);
        assert_eq!(capture.state.regs.rip, XSAVE_ENDPOINT_RIP as u64);
        assert_eq!(capture.state.regs.rbx, 1);
        assert_eq!(capture.state.xcrs.xcr0, 3);
        assert!(
            xsave_output_page(&capture.memory)
                .iter()
                .any(|&byte| byte != 0),
            "guest XSAVE must write the zeroed output page"
        );
        capture
    }

    #[test]
    #[ignore = "live KVM; run with --ignored and XSAVE_CONTINUATION_REPORT_DIR"]
    fn xsave_guest_bytes_survive_cold_continuation() {
        require_kvm();
        let report = report_root("XSAVE_CONTINUATION_REPORT_DIR");

        let mut uninterrupted = fresh_xsave_vmm(true);
        wire_snapshot_path(&mut uninterrupted);
        run_xsave_warmup(&mut uninterrupted);
        let uninterrupted_endpoint = run_xsave_endpoint(&mut uninterrupted);
        retain_capture(
            report.as_deref(),
            "original-endpoint",
            &uninterrupted_endpoint,
            xsave_output_page(&uninterrupted_endpoint.memory),
        );

        let mut save_and_continue = fresh_xsave_vmm(true);
        wire_snapshot_path(&mut save_and_continue);
        run_xsave_warmup(&mut save_and_continue);
        let save_before_memory = save_and_continue.guest_memory().to_vec();
        let save_before_serial = save_and_continue.serial().to_vec();
        let save_before_counts = save_and_continue.exit_counts();
        let save_before_vns = save_and_continue.effective_vns();
        let save_stop = capture_full_vmm(&save_and_continue);
        retain_capture(
            report.as_deref(),
            "save-and-continue-stop",
            &save_stop,
            xsave_output_page(&save_stop.memory),
        );
        assert_eq!(
            save_and_continue.guest_memory(),
            save_before_memory.as_slice()
        );
        assert_eq!(save_and_continue.serial(), save_before_serial.as_slice());
        assert_eq!(save_and_continue.exit_counts(), save_before_counts);
        assert_eq!(save_and_continue.effective_vns(), save_before_vns);
        assert!(
            xsave_output_page(&save_stop.memory)
                .iter()
                .all(|&byte| byte == 0),
            "XSAVE output must be empty at the saved UART boundary"
        );
        let save_repeated = capture_full_vmm(&save_and_continue);
        retain_capture(
            report.as_deref(),
            "save-and-continue-stop-repeat",
            &save_repeated,
            xsave_output_page(&save_repeated.memory),
        );
        assert!(
            save_stop == save_repeated,
            "XSAVE saved stop differs on repeat; retained XSAVE evidence is available"
        );
        assert_eq!(save_stop.state.xcrs.xcr0, 3);
        let save_and_continue_endpoint = run_xsave_endpoint(&mut save_and_continue);
        retain_capture(
            report.as_deref(),
            "save-and-continue-endpoint",
            &save_and_continue_endpoint,
            xsave_output_page(&save_and_continue_endpoint.memory),
        );

        let mut cold_unobserved = fresh_xsave_vmm(false);
        wire_snapshot_path(&mut cold_unobserved);
        cold_unobserved
            .restore_snapshot(&save_stop.memory, &save_stop.state)
            .expect("restore XSAVE continuation");
        let cold_unobserved_endpoint = run_xsave_endpoint(&mut cold_unobserved);
        retain_capture(
            report.as_deref(),
            "cold-unobserved-endpoint",
            &cold_unobserved_endpoint,
            xsave_output_page(&cold_unobserved_endpoint.memory),
        );

        let mut cold_observed = fresh_xsave_vmm(false);
        wire_snapshot_path(&mut cold_observed);
        cold_observed
            .restore_snapshot(&save_stop.memory, &save_stop.state)
            .expect("restore observed XSAVE continuation");
        let cold_before_memory = cold_observed.guest_memory().to_vec();
        let cold_before_serial = cold_observed.serial().to_vec();
        let cold_before_counts = cold_observed.exit_counts();
        let cold_before_vns = cold_observed.effective_vns();
        let cold_stop = capture_full_vmm(&cold_observed);
        retain_capture(
            report.as_deref(),
            "cold-observed-stop",
            &cold_stop,
            xsave_output_page(&cold_stop.memory),
        );
        assert_eq!(cold_observed.guest_memory(), cold_before_memory.as_slice());
        assert_eq!(cold_observed.serial(), cold_before_serial.as_slice());
        assert_eq!(cold_observed.exit_counts(), cold_before_counts);
        assert_eq!(cold_observed.effective_vns(), cold_before_vns);
        let cold_repeated = capture_full_vmm(&cold_observed);
        retain_capture(
            report.as_deref(),
            "cold-observed-stop-repeat",
            &cold_repeated,
            xsave_output_page(&cold_repeated.memory),
        );
        assert!(
            cold_stop == cold_repeated,
            "XSAVE cold stop differs on repeat; retained XSAVE evidence is available"
        );
        assert!(
            cold_stop == save_stop,
            "XSAVE cold stop differs from saved stop; retained XSAVE evidence is available"
        );
        let cold_observed_endpoint = run_xsave_endpoint(&mut cold_observed);
        retain_capture(
            report.as_deref(),
            "cold-observed-endpoint",
            &cold_observed_endpoint,
            xsave_output_page(&cold_observed_endpoint.memory),
        );

        assert!(
            uninterrupted_endpoint == save_and_continue_endpoint,
            "uninterrupted and save-and-continue XSAVE futures differ; retained XSAVE evidence is available"
        );
        assert!(
            save_and_continue_endpoint == cold_unobserved_endpoint,
            "cold XSAVE continuation differs; retained XSAVE evidence is available"
        );
        assert!(
            cold_unobserved_endpoint == cold_observed_endpoint,
            "observing cold XSAVE state changes its future; retained XSAVE evidence is available"
        );
    }
}
