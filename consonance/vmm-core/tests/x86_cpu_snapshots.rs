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
        assert_eq!(repeated, save_stop, "repeated full snapshot is unchanged");
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

        assert_eq!(uninterrupted_stop, save_stop);
        assert_eq!(uninterrupted_stop, cold_stop);
        assert_eq!(
            uninterrupted_endpoint, save_and_continue_endpoint,
            "uninterrupted and save-and-continue endpoints must match"
        );
        assert_eq!(
            save_and_continue_endpoint, cold_endpoint,
            "cold restore continuation must match the source endpoint"
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

    #[test]
    #[ignore = "live AMD KVM NPT; run with --ignored on an AuthenticAMD /dev/kvm host"]
    fn amd_default_npt_pae_snapshot_identity() {
        require_kvm();

        let cpuinfo = std::fs::read_to_string("/proc/cpuinfo")
            .expect("read /proc/cpuinfo for the AMD NPT gate");
        assert!(
            cpuinfo.lines().any(|line| {
                line.split_once(':').is_some_and(|(key, value)| {
                    key.trim() == "vendor_id" && value.trim() == "AuthenticAMD"
                })
            }),
            "this diagnostic requires an AuthenticAMD host"
        );
        let npt = std::fs::read_to_string("/sys/module/kvm_amd/parameters/npt")
            .expect("read kvm_amd NPT setting");
        assert!(
            matches!(npt.trim(), "Y" | "1"),
            "this diagnostic requires kvm_amd NPT to be enabled, got {:?}",
            npt.trim()
        );

        // Keep this warmup deliberately observationally minimal. In particular,
        // do not read vCPU state or save/hash before the first continuation: the
        // NPT transition itself is what the first endpoint must expose.
        let warmup = |vmm: &mut Vmm<KvmBackend>| {
            assert_eq!(
                vmm.step().expect("run AMD NPT scalar UART warmup"),
                Step::Continued,
                "the warmup UART OUT is a serviced, non-terminal PIO exit"
            );
            assert_eq!(vmm.serial(), &[WARMUP_MARKER]);
            assert_eq!(vmm.exit_counts().io, 1);
            assert_eq!(vmm.exit_counts().total(), 1);
        };

        // The uninterrupted arm establishes the NPT endpoint after RAM changes
        // the PDPT. The expected B mapping is intentional: a disagreement is
        // hardware evidence, not a reason to derive the expectation from output.
        let mut uninterrupted = fresh_vmm(true);
        wire_snapshot_path(&mut uninterrupted);
        warmup(&mut uninterrupted);
        switch_guest_pdpt_to_b(&mut uninterrupted);
        let uninterrupted_endpoint = run_bounded(&mut uninterrupted, 0x99, PDPT_B_ENTRY);

        // Capture the same boundary without making an earlier CPU save. The
        // direct observations below establish that capture itself does not
        // change RAM, UART, exit accounting, or virtual time.
        let mut save_and_continue = fresh_vmm(true);
        wire_snapshot_path(&mut save_and_continue);
        warmup(&mut save_and_continue);
        switch_guest_pdpt_to_b(&mut save_and_continue);
        let before_memory = save_and_continue.guest_memory().to_vec();
        let before_serial = save_and_continue.serial().to_vec();
        let before_counts = save_and_continue.exit_counts();
        let before_vns = save_and_continue.effective_vns();
        let capture = capture_full_vmm(&save_and_continue);
        assert_eq!(capture.state.regs.rip, WARMUP_RIP as u64);
        assert_eq!(capture.state.regs.rbx, 0);
        assert_eq!(capture.state.sregs.pdptrs[0], PDPT_B_ENTRY);
        assert_eq!(
            capture.memory[PDPT_GPA..PDPT_GPA + 8],
            PDPT_B_ENTRY.to_le_bytes(),
            "the captured RAM must retain the NPT-selected PDPT B entry"
        );
        assert_eq!(save_and_continue.guest_memory(), before_memory.as_slice());
        assert_eq!(save_and_continue.serial(), before_serial.as_slice());
        assert_eq!(save_and_continue.exit_counts(), before_counts);
        assert_eq!(save_and_continue.effective_vns(), before_vns);
        let repeated = capture_full_vmm(&save_and_continue);
        assert_eq!(repeated, capture, "repeated capture is byte-identical");
        let save_and_continue_endpoint = run_bounded(&mut save_and_continue, 0x99, PDPT_B_ENTRY);

        // Restore the complete source into a fresh production-composed VMM.
        let mut cold = fresh_vmm(false);
        wire_snapshot_path(&mut cold);
        cold.restore_snapshot(&capture.memory, &capture.state)
            .expect("restore AMD NPT full VMM snapshot");
        let cold_stop = capture_full_vmm(&cold);
        assert_eq!(
            cold_stop, capture,
            "cold restore reproduces the saved boundary"
        );
        let cold_endpoint = run_bounded(&mut cold, 0x99, PDPT_B_ENTRY);

        assert_eq!(
            uninterrupted_endpoint, save_and_continue_endpoint,
            "uninterrupted and save-and-continue NPT endpoints must match"
        );
        assert_eq!(
            save_and_continue_endpoint, cold_endpoint,
            "cold restore must reproduce the NPT endpoint"
        );

        // Negative control: retain the saved CPU state but restore the original
        // RAM image, whose PDPT still points at A. This tests that the accepted
        // endpoint's guest RAM is necessary under NPT; it does not clear the
        // saved PDPTR validity bit or test the cached-PDPTR behavior.
        let original_memory = guest_ram_image().as_bytes().to_vec();
        let mut negative = fresh_vmm(false);
        wire_snapshot_path(&mut negative);
        negative
            .restore_snapshot(&original_memory, &capture.state)
            .expect("restore NPT negative-control RAM image");
        let negative_endpoint = run_bounded(&mut negative, 0x42, PDPT_A_ENTRY);
        assert_ne!(
            negative_endpoint, uninterrupted_endpoint,
            "changing only the restored NPT guest RAM must change the endpoint"
        );
        println!(
            "AMD NPT snapshot witness: positive serial={:?} vtime={:?} hash={:?}; negative serial={:?} vtime={:?} hash={:?}",
            uninterrupted_endpoint.serial,
            uninterrupted_endpoint.effective_vns,
            uninterrupted_endpoint.state_hash,
            negative_endpoint.serial,
            negative_endpoint.effective_vns,
            negative_endpoint.state_hash
        );
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct NptObservedEndpoint {
        endpoint: Endpoint,
        pdptr_flags: u64,
        pdptrs: [u64; 4],
        rip: u64,
        rbx: u64,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct NptObservedStop {
        capture: MmioCapture,
        serial: Vec<u8>,
        pdptr_flags: u64,
        pdptrs: [u64; 4],
        rip: u64,
        rbx: u64,
    }

    fn run_npt_observed(vmm: &mut Vmm<KvmBackend>) -> NptObservedEndpoint {
        let mut steps = 0;
        loop {
            assert!(steps < MAX_STEPS, "AMD NPT guest exceeded the step budget");
            match vmm.step().expect("run one AMD NPT guest exit") {
                Step::Continued => steps += 1,
                Step::Terminal(reason) => {
                    assert_eq!(reason, TerminalReason::Idle, "AMD NPT guest stopped at HLT");
                    break;
                }
                Step::SdkStop => panic!("unexpected SDK stop in AMD NPT guest"),
            }
        }

        let serial = vmm.serial().to_vec();
        assert_eq!(
            serial.len(),
            2,
            "AMD NPT endpoint must contain two UART bytes"
        );
        assert_eq!(serial[0], WARMUP_MARKER);
        let state = vmm.save_vm_state().expect("save AMD NPT endpoint VM state");
        assert_eq!(state.regs.rbx, 1, "AMD NPT endpoint must retire INC EBX");
        let encoded_state = state.encode().expect("encode AMD NPT endpoint VM state");
        NptObservedEndpoint {
            endpoint: Endpoint {
                serial,
                memory: vmm.guest_memory().to_vec(),
                encoded_state,
                state_blob: vmm
                    .state_blob()
                    .expect("encode AMD NPT endpoint state blob"),
                state_hash: vmm.state_hash().expect("hash AMD NPT endpoint state"),
                effective_vns: vmm.effective_vns(),
            },
            pdptr_flags: state.sregs.flags,
            pdptrs: state.sregs.pdptrs,
            rip: state.regs.rip,
            rbx: state.regs.rbx,
        }
    }

    fn capture_npt_stop(vmm: &Vmm<KvmBackend>) -> NptObservedStop {
        let capture = capture_full_vmm(vmm);
        NptObservedStop {
            serial: vmm.serial().to_vec(),
            pdptr_flags: capture.state.sregs.flags,
            pdptrs: capture.state.sregs.pdptrs,
            rip: capture.state.regs.rip,
            rbx: capture.state.regs.rbx,
            capture,
        }
    }

    fn npt_hex(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for &byte in bytes {
            out.push(HEX[usize::from(byte >> 4)] as char);
            out.push(HEX[usize::from(byte & 0x0f)] as char);
        }
        out
    }

    fn write_npt_file(path: &std::path::Path, bytes: &[u8]) {
        use std::io::Write;

        let parent = path.parent().expect("diagnostic file has a parent");
        std::fs::create_dir_all(parent).expect("create NPT diagnostic directory");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("create immutable NPT diagnostic file");
        file.write_all(bytes)
            .expect("write immutable NPT diagnostic file");
        file.sync_all().expect("sync immutable NPT diagnostic file");
    }

    #[allow(clippy::too_many_arguments)]
    fn write_npt_artifacts(
        root: &std::path::Path,
        arm: &str,
        phase: &str,
        memory: &[u8],
        vm_state: &[u8],
        state_blob: &[u8],
        serial: &[u8],
        effective_vns: Option<u64>,
        state_hash: &[u8; 32],
        pdptr_flags: u64,
        pdptrs: [u64; 4],
        rip: u64,
        rbx: u64,
    ) {
        let directory = root.join(arm).join(phase);
        write_npt_file(&directory.join("memory.bin"), memory);
        write_npt_file(&directory.join("vm-state.bin"), vm_state);
        write_npt_file(&directory.join("state-blob.bin"), state_blob);
        let serial_json = serial
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let pdptrs_json = pdptrs
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let time_json = effective_vns.map_or_else(|| "null".to_owned(), |time| time.to_string());
        let summary = format!(
            "{{\"arm\":\"{arm}\",\"phase\":\"{phase}\",\"serial\":[{serial_json}],\"virtual_time\":{time_json},\"state_hash\":\"{}\",\"pdptr_flags\":{pdptr_flags},\"pdptrs\":[{pdptrs_json}],\"rip\":{rip},\"rbx\":{rbx}}}\n",
            npt_hex(state_hash)
        );
        write_npt_file(&directory.join("summary.json"), summary.as_bytes());
    }

    fn write_npt_endpoint(root: &std::path::Path, arm: &str, observed: &NptObservedEndpoint) {
        let endpoint = &observed.endpoint;
        write_npt_artifacts(
            root,
            arm,
            "endpoint",
            &endpoint.memory,
            &endpoint.encoded_state,
            &endpoint.state_blob,
            &endpoint.serial,
            endpoint.effective_vns,
            &endpoint.state_hash,
            observed.pdptr_flags,
            observed.pdptrs,
            observed.rip,
            observed.rbx,
        );
    }

    fn write_npt_stop(root: &std::path::Path, arm: &str, phase: &str, observed: &NptObservedStop) {
        let capture = &observed.capture;
        write_npt_artifacts(
            root,
            arm,
            phase,
            &capture.memory,
            &capture.encoded_state,
            &capture.state_blob,
            &observed.serial,
            capture.moment,
            &capture.state_hash,
            observed.pdptr_flags,
            observed.pdptrs,
            observed.rip,
            observed.rbx,
        );
    }

    #[test]
    #[ignore = "diagnostic live AMD KVM NPT; run with --ignored and NPT_REPORT_DIR"]
    fn amd_default_npt_pae_observations() {
        require_kvm();

        let cpuinfo = std::fs::read_to_string("/proc/cpuinfo")
            .expect("read /proc/cpuinfo for the AMD NPT diagnostic");
        assert!(
            cpuinfo.lines().any(|line| {
                line.split_once(':').is_some_and(|(key, value)| {
                    key.trim() == "vendor_id" && value.trim() == "AuthenticAMD"
                })
            }),
            "this diagnostic requires an AuthenticAMD host"
        );
        let npt = std::fs::read_to_string("/sys/module/kvm_amd/parameters/npt")
            .expect("read kvm_amd NPT setting");
        assert!(
            matches!(npt.trim(), "Y" | "1"),
            "this diagnostic requires kvm_amd NPT to be enabled, got {:?}",
            npt.trim()
        );
        let report_root = std::path::PathBuf::from(
            std::env::var_os("NPT_REPORT_DIR")
                .expect("NPT_REPORT_DIR must identify the diagnostic output directory"),
        );
        std::fs::create_dir_all(&report_root).expect("create NPT diagnostic output directory");

        // Keep the warmup observationally minimal. No vCPU read or save/hash is
        // allowed before the first arm continues beyond the RAM-only transition.
        let warmup = |vmm: &mut Vmm<KvmBackend>| {
            assert_eq!(
                vmm.step().expect("run AMD NPT scalar UART warmup"),
                Step::Continued,
                "the warmup UART OUT is a serviced, non-terminal PIO exit"
            );
            assert_eq!(vmm.serial(), &[WARMUP_MARKER]);
            assert_eq!(vmm.exit_counts().io, 1);
            assert_eq!(vmm.exit_counts().total(), 1);
        };

        let mut uninterrupted = fresh_vmm(true);
        wire_snapshot_path(&mut uninterrupted);
        warmup(&mut uninterrupted);
        switch_guest_pdpt_to_b(&mut uninterrupted);
        let uninterrupted_endpoint = run_npt_observed(&mut uninterrupted);
        write_npt_endpoint(&report_root, "original", &uninterrupted_endpoint);

        let mut save_and_continue = fresh_vmm(true);
        wire_snapshot_path(&mut save_and_continue);
        warmup(&mut save_and_continue);
        switch_guest_pdpt_to_b(&mut save_and_continue);
        let save_before_memory = save_and_continue.guest_memory().to_vec();
        let save_before_serial = save_and_continue.serial().to_vec();
        let save_before_counts = save_and_continue.exit_counts();
        let save_before_vns = save_and_continue.effective_vns();
        let save_stop = capture_npt_stop(&save_and_continue);
        let save_repeated = capture_npt_stop(&save_and_continue);
        let save_after_memory = save_and_continue.guest_memory().to_vec();
        let save_after_serial = save_and_continue.serial().to_vec();
        let save_after_counts = save_and_continue.exit_counts();
        let save_after_vns = save_and_continue.effective_vns();
        let save_and_continue_endpoint = run_npt_observed(&mut save_and_continue);
        write_npt_stop(&report_root, "save-and-continue", "stop", &save_stop);
        write_npt_stop(
            &report_root,
            "save-and-continue",
            "stop-repeat",
            &save_repeated,
        );
        write_npt_endpoint(
            &report_root,
            "save-and-continue",
            &save_and_continue_endpoint,
        );

        let mut cold_unobserved = fresh_vmm(false);
        wire_snapshot_path(&mut cold_unobserved);
        cold_unobserved
            .restore_snapshot(&save_stop.capture.memory, &save_stop.capture.state)
            .expect("restore AMD NPT unobserved stop");
        // This arm intentionally runs directly after restore; it does not read
        // the stopped CPU before establishing its continuation endpoint.
        let cold_unobserved_endpoint = run_npt_observed(&mut cold_unobserved);
        write_npt_endpoint(&report_root, "cold-unobserved", &cold_unobserved_endpoint);

        let mut cold_observed = fresh_vmm(false);
        wire_snapshot_path(&mut cold_observed);
        cold_observed
            .restore_snapshot(&save_stop.capture.memory, &save_stop.capture.state)
            .expect("restore AMD NPT observed stop");
        let cold_before_memory = cold_observed.guest_memory().to_vec();
        let cold_before_serial = cold_observed.serial().to_vec();
        let cold_before_counts = cold_observed.exit_counts();
        let cold_before_vns = cold_observed.effective_vns();
        let cold_observed_stop = capture_npt_stop(&cold_observed);
        let cold_observed_repeated = capture_npt_stop(&cold_observed);
        let cold_after_memory = cold_observed.guest_memory().to_vec();
        let cold_after_serial = cold_observed.serial().to_vec();
        let cold_after_counts = cold_observed.exit_counts();
        let cold_after_vns = cold_observed.effective_vns();
        let cold_observed_endpoint = run_npt_observed(&mut cold_observed);
        write_npt_stop(&report_root, "cold-observed", "stop", &cold_observed_stop);
        write_npt_stop(
            &report_root,
            "cold-observed",
            "stop-repeat",
            &cold_observed_repeated,
        );
        write_npt_endpoint(&report_root, "cold-observed", &cold_observed_endpoint);

        let original_memory = guest_ram_image().as_bytes().to_vec();
        let mut wrong_original_ram = fresh_vmm(false);
        wire_snapshot_path(&mut wrong_original_ram);
        wrong_original_ram
            .restore_snapshot(&original_memory, &save_stop.capture.state)
            .expect("restore AMD NPT original-RAM diagnostic");
        let wrong_original_ram_endpoint = run_npt_observed(&mut wrong_original_ram);
        write_npt_endpoint(
            &report_root,
            "wrong-original-ram",
            &wrong_original_ram_endpoint,
        );

        assert!(
            save_stop.rip == WARMUP_RIP as u64,
            "saved stop RIP must be the warmup boundary"
        );
        assert!(save_stop.rbx == 0, "saved stop RBX must precede INC EBX");
        assert!(
            save_stop.capture == save_repeated.capture,
            "repeated saved-stop capture must be identical"
        );
        assert!(
            save_after_memory == save_before_memory,
            "saved capture must not change RAM"
        );
        assert!(
            save_after_serial == save_before_serial,
            "saved capture must not change UART"
        );
        assert!(
            save_after_counts == save_before_counts,
            "saved capture must not change exits"
        );
        assert!(
            save_after_vns == save_before_vns,
            "saved capture must not change virtual time"
        );
        assert!(
            cold_observed_stop.capture == cold_observed_repeated.capture,
            "repeated cold-stop capture must be identical"
        );
        assert!(
            cold_observed_stop == save_stop,
            "cold observed stop must reproduce the saved stop"
        );
        assert!(
            cold_after_memory == cold_before_memory,
            "cold capture must not change RAM"
        );
        assert!(
            cold_after_serial == cold_before_serial,
            "cold capture must not change UART"
        );
        assert!(
            cold_after_counts == cold_before_counts,
            "cold capture must not change exits"
        );
        assert!(
            cold_after_vns == cold_before_vns,
            "cold capture must not change virtual time"
        );
        assert!(
            uninterrupted_endpoint == save_and_continue_endpoint,
            "original and save-and-continue full NPT endpoints must match"
        );
        assert!(
            save_and_continue_endpoint == cold_unobserved_endpoint,
            "cold unobserved continuation must match the full endpoint"
        );
        assert!(
            cold_unobserved_endpoint == cold_observed_endpoint,
            "cold observed continuation must match the full endpoint"
        );
        assert!(
            wrong_original_ram_endpoint.endpoint != uninterrupted_endpoint.endpoint,
            "wrong original RAM is diagnostic-only and must differ"
        );
        println!(
            "AMD NPT observations: original={:?} save={:?} cold-unobserved={:?} cold-observed={:?} wrong-RAM={:?}",
            uninterrupted_endpoint.endpoint.state_hash,
            save_and_continue_endpoint.endpoint.state_hash,
            cold_unobserved_endpoint.endpoint.state_hash,
            cold_observed_endpoint.endpoint.state_hash,
            wrong_original_ram_endpoint.endpoint.state_hash
        );
    }
}
