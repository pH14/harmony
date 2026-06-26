// SPDX-License-Identifier: AGPL-3.0-or-later
//! Non-`#[ignore]` unit tests for the pure KVM mapping logic (`super`), driven by
//! **synthetic `kvm_run`/`kvm_*` structs** — no `/dev/kvm`, no ioctl. They run on
//! the Linux CI runner (so `cargo llvm-cov` / `cargo mutants --in-diff` exercise
//! the decode/apply seam, the `kvm_bindings` ⇄ `VcpuState` conversions, and the
//! snapshot/CPUID/MSR/capability helpers) and under Miri (which scrutinizes the
//! raw `kvm_run` access for UB). The box-only syscall orchestration in
//! `kvm_sys` is what stays excluded from those gates.

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::collections::BTreeMap;

use kvm_bindings::{
    KVM_EXIT_FAIL_ENTRY, KVM_EXIT_HLT, KVM_EXIT_INTERNAL_ERROR, KVM_EXIT_IO,
    KVM_EXIT_IRQ_WINDOW_OPEN, KVM_EXIT_MMIO, KVM_EXIT_SHUTDOWN, KVM_EXIT_X86_RDMSR,
    KVM_EXIT_X86_WRMSR, KVM_MP_STATE_HALTED, KVM_MP_STATE_RUNNABLE, kvm_msr_entry, kvm_run,
};

use super::*;
use crate::config::{CpuidEntry, CpuidModel, MsrFilter, MsrRange};
use crate::exit::Exit;
use crate::state::{DebugRegs, DescriptorTable, MpState, Segment, VcpuEvents, VcpuRegs, VcpuSregs};
use crate::types::Gpa;

// ---------------------------------------------------------------------------
// decode_* / apply_* over a synthetic kvm_run buffer.
// ---------------------------------------------------------------------------

/// A page-aligned, zeroed buffer large enough to hold a `kvm_run` plus a PIO data
/// area, reached only through its raw pointer (the production shape).
struct SynRun {
    ptr: *mut u8,
    layout: Layout,
    len: usize,
}

/// Where synthetic PIO data lives — comfortably past `size_of::<kvm_run>()`.
const PIO_OFF: usize = 8192;

impl SynRun {
    fn new() -> Self {
        let len = 16384;
        assert!(
            size_of::<kvm_run>() <= PIO_OFF,
            "PIO area must clear kvm_run"
        );
        let layout = Layout::from_size_align(len, 4096).expect("layout");
        // SAFETY: non-zero size, power-of-two align.
        let ptr = unsafe { alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "alloc failed");
        Self { ptr, layout, len }
    }
    fn run(&self) -> *mut kvm_run {
        self.ptr.cast::<kvm_run>()
    }
    fn page(&self) -> RunPage {
        // SAFETY: `ptr` backs `len` live zeroed bytes aligned for `kvm_run`.
        unsafe { RunPage::new(self.run(), self.len) }
    }
    fn byte(&self, off: usize) -> u8 {
        // SAFETY: `off < len`.
        unsafe { *self.ptr.add(off) }
    }
    fn set_byte(&self, off: usize, v: u8) {
        // SAFETY: `off < len`.
        unsafe { *self.ptr.add(off) = v };
    }
}
impl Drop for SynRun {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`layout` from `alloc_zeroed`; freed once.
        unsafe { dealloc(self.ptr, self.layout) };
    }
}

fn set_reason(s: &SynRun, reason: u32) {
    // SAFETY: `run()` is a valid, owned `kvm_run`.
    unsafe { (*s.run()).exit_reason = reason };
}

#[test]
fn decode_io_out_reads_value_via_run_buf() {
    let s = SynRun::new();
    set_reason(&s, KVM_EXIT_IO);
    // SAFETY: writing union sub-fields of an owned, zeroed kvm_run.
    unsafe {
        let io = &mut (*s.run()).__bindgen_anon_1.io;
        io.direction = 1; // OUT
        io.size = 1;
        io.port = 0x3F8;
        io.count = 1;
        io.data_offset = PIO_OFF as u64;
    }
    s.set_byte(PIO_OFF, 0x42);

    let (exit, pending) = decode_exit(s.page()).unwrap().unwrap();
    assert_eq!(
        exit,
        Exit::Io {
            port: 0x3F8,
            size: 1,
            write: Some(0x42)
        }
    );
    assert_eq!(pending, Pending::None);
}

#[test]
fn decode_io_in_arms_pending() {
    let s = SynRun::new();
    set_reason(&s, KVM_EXIT_IO);
    // SAFETY: union sub-field writes.
    unsafe {
        let io = &mut (*s.run()).__bindgen_anon_1.io;
        io.direction = 0; // IN
        io.size = 2;
        io.port = 0x60;
        io.count = 1;
        io.data_offset = PIO_OFF as u64;
    }
    let (exit, pending) = decode_exit(s.page()).unwrap().unwrap();
    assert_eq!(
        exit,
        Exit::Io {
            port: 0x60,
            size: 2,
            write: None
        }
    );
    assert_eq!(
        pending,
        Pending::IoIn {
            data_offset: PIO_OFF as u64,
            size: 2
        }
    );
}

#[test]
fn decode_io_rep_string_fails_closed() {
    let s = SynRun::new();
    set_reason(&s, KVM_EXIT_IO);
    // SAFETY: union sub-field writes.
    unsafe {
        let io = &mut (*s.run()).__bindgen_anon_1.io;
        io.direction = 1;
        io.size = 1;
        io.port = 0x3F8;
        io.count = 7; // string/REP PIO
        io.data_offset = PIO_OFF as u64;
    }
    assert!(matches!(
        decode_exit(s.page()),
        Err(BackendError::Unsupported {
            what: "string/REP port I/O (io.count != 1)"
        })
    ));
}

#[test]
fn decode_io_out_offset_past_page_is_error_not_ub() {
    let s = SynRun::new();
    set_reason(&s, KVM_EXIT_IO);
    // SAFETY: union sub-field writes.
    unsafe {
        let io = &mut (*s.run()).__bindgen_anon_1.io;
        io.direction = 1;
        io.size = 4;
        io.port = 0x3F8;
        io.count = 1;
        io.data_offset = (s.len as u64) - 2; // 4-byte read would cross the end
    }
    // The bounded run_buf seam rejects it (Miri would flag a missed check).
    assert!(matches!(
        decode_exit(s.page()),
        Err(BackendError::Memory(_))
    ));
}

#[test]
fn decode_mmio_store_and_load() {
    // store
    let s = SynRun::new();
    set_reason(&s, KVM_EXIT_MMIO);
    // SAFETY: union sub-field writes.
    unsafe {
        let m = &mut (*s.run()).__bindgen_anon_1.mmio;
        m.phys_addr = 0xFEE0_0000;
        m.len = 4;
        m.is_write = 1;
        m.data[..4].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    }
    let (exit, pending) = decode_exit(s.page()).unwrap().unwrap();
    assert_eq!(
        exit,
        Exit::Mmio {
            gpa: Gpa(0xFEE0_0000),
            size: 4,
            write: Some(0x1234_5678)
        }
    );
    assert_eq!(pending, Pending::None);

    // load
    let s = SynRun::new();
    set_reason(&s, KVM_EXIT_MMIO);
    // SAFETY: union sub-field writes.
    unsafe {
        let m = &mut (*s.run()).__bindgen_anon_1.mmio;
        m.phys_addr = 0xFEE0_0080;
        m.len = 4;
        m.is_write = 0;
    }
    let (exit, pending) = decode_exit(s.page()).unwrap().unwrap();
    assert_eq!(
        exit,
        Exit::Mmio {
            gpa: Gpa(0xFEE0_0080),
            size: 4,
            write: None
        }
    );
    assert_eq!(pending, Pending::MmioLoad { len: 4 });
}

#[test]
fn decode_rdmsr_and_wrmsr() {
    let s = SynRun::new();
    set_reason(&s, KVM_EXIT_X86_RDMSR);
    // SAFETY: union sub-field writes.
    unsafe { (*s.run()).__bindgen_anon_1.msr.index = 0x1B };
    let (exit, pending) = decode_exit(s.page()).unwrap().unwrap();
    assert_eq!(exit, Exit::Rdmsr { index: 0x1B });
    assert_eq!(pending, Pending::Rdmsr);

    let s = SynRun::new();
    set_reason(&s, KVM_EXIT_X86_WRMSR);
    // SAFETY: union sub-field writes.
    unsafe {
        let m = &mut (*s.run()).__bindgen_anon_1.msr;
        m.index = 0x6E0;
        m.data = 0xDEAD_BEEF;
    }
    let (exit, pending) = decode_exit(s.page()).unwrap().unwrap();
    assert_eq!(
        exit,
        Exit::Wrmsr {
            index: 0x6E0,
            value: 0xDEAD_BEEF
        }
    );
    assert_eq!(pending, Pending::Wrmsr);
}

#[test]
fn decode_terminal_and_control_exits() {
    for (reason, want) in [
        (KVM_EXIT_HLT, Exit::Hlt),
        (KVM_EXIT_SHUTDOWN, Exit::Shutdown),
    ] {
        let s = SynRun::new();
        set_reason(&s, reason);
        let (exit, pending) = decode_exit(s.page()).unwrap().unwrap();
        assert_eq!(exit, want);
        assert_eq!(pending, Pending::None);
    }
    // IRQ-window is a control exit consumed internally (None, re-enter).
    let s = SynRun::new();
    set_reason(&s, KVM_EXIT_IRQ_WINDOW_OPEN);
    assert_eq!(decode_exit(s.page()).unwrap(), None);
}

#[test]
fn decode_error_and_unknown_exits_fail_closed() {
    // Each fail-closed reason carries its own distinct message (so each match arm
    // is load-bearing, not collapsible into the `_` arm).
    for (reason, msg) in [
        (KVM_EXIT_INTERNAL_ERROR, "KVM_EXIT_INTERNAL_ERROR"),
        (KVM_EXIT_FAIL_ENTRY, "KVM_EXIT_FAIL_ENTRY"),
        (0xDEAD_BEEF, "unhandled KVM exit reason"),
    ] {
        let s = SynRun::new();
        set_reason(&s, reason);
        match decode_exit(s.page()) {
            Err(BackendError::Internal(got)) => assert_eq!(got, msg),
            other => panic!("expected Internal({msg:?}), got {other:?}"),
        }
    }
}

#[test]
fn apply_complete_read_routes_by_pending() {
    // IoIn → writes the value into the PIO data buffer.
    let s = SynRun::new();
    apply_complete_read(
        s.page(),
        Pending::IoIn {
            data_offset: PIO_OFF as u64,
            size: 1,
        },
        0x55,
    )
    .unwrap();
    assert_eq!(s.byte(PIO_OFF), 0x55);

    // MmioLoad → writes mmio.data.
    let s = SynRun::new();
    apply_complete_read(s.page(), Pending::MmioLoad { len: 4 }, 0xAABB_CCDD).unwrap();
    // SAFETY: read back the union member just written.
    let data = unsafe { (*s.run()).__bindgen_anon_1.mmio.data };
    assert_eq!(&data[..4], &0xAABB_CCDDu32.to_le_bytes());

    // Rdmsr → sets msr.data and clears error.
    let s = SynRun::new();
    apply_complete_read(s.page(), Pending::Rdmsr, 0x1234).unwrap();
    // SAFETY: read back the union member just written.
    let msr = unsafe { (*s.run()).__bindgen_anon_1.msr };
    assert_eq!(msr.data, 0x1234);
    assert_eq!(msr.error, 0);

    // Wrong pending → NoPendingRead, nothing written.
    let s = SynRun::new();
    assert!(matches!(
        apply_complete_read(s.page(), Pending::Wrmsr, 1),
        Err(BackendError::NoPendingRead)
    ));
    assert!(matches!(
        apply_complete_read(s.page(), Pending::None, 1),
        Err(BackendError::NoPendingRead)
    ));
}

#[test]
fn apply_complete_fault_and_ok_set_msr_error() {
    // fault on RDMSR/WRMSR → error = 1.
    for p in [Pending::Rdmsr, Pending::Wrmsr] {
        let s = SynRun::new();
        apply_complete_fault(s.page(), p).unwrap();
        // SAFETY: read back the union member.
        assert_eq!(unsafe { (*s.run()).__bindgen_anon_1.msr.error }, 1);
    }
    // ok on WRMSR → error = 0 (pre-set to prove it is cleared).
    let s = SynRun::new();
    // SAFETY: union sub-field write.
    unsafe { (*s.run()).__bindgen_anon_1.msr.error = 9 };
    apply_complete_ok(s.page(), Pending::Wrmsr).unwrap();
    assert_eq!(unsafe { (*s.run()).__bindgen_anon_1.msr.error }, 0);

    // Mismatched pending → BadCompletion.
    let s = SynRun::new();
    assert!(matches!(
        apply_complete_fault(
            s.page(),
            Pending::IoIn {
                data_offset: 0,
                size: 1
            }
        ),
        Err(BackendError::BadCompletion)
    ));
    assert!(matches!(
        apply_complete_ok(s.page(), Pending::Rdmsr),
        Err(BackendError::BadCompletion)
    ));
}

// ---------------------------------------------------------------------------
// Config / snapshot helpers.
// ---------------------------------------------------------------------------

#[test]
fn cpuid_entries_maps_fields_and_significant_flag() {
    let model = CpuidModel {
        entries: vec![
            CpuidEntry {
                leaf: 1,
                subleaf: 0,
                subleaf_significant: false,
                eax: 0xA,
                ebx: 0xB,
                ecx: 0xC,
                edx: 0xD,
            },
            CpuidEntry {
                leaf: 0xD,
                subleaf: 1,
                subleaf_significant: true,
                eax: 1,
                ebx: 2,
                ecx: 3,
                edx: 4,
            },
        ],
    };
    let out = cpuid_entries(&model);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].function, 1);
    assert_eq!(out[0].index, 0);
    assert_eq!(out[0].flags, 0);
    assert_eq!(
        (out[0].eax, out[0].ebx, out[0].ecx, out[0].edx),
        (0xA, 0xB, 0xC, 0xD)
    );
    assert_eq!(out[1].function, 0xD);
    assert_eq!(out[1].index, 1);
    assert_eq!(out[1].flags, kvm_bindings::KVM_CPUID_FLAG_SIGNIFCANT_INDEX);
}

#[test]
fn msr_count_checks_fail_closed_on_short_count() {
    assert!(ensure_full_msr_count(3, 3).is_ok());
    assert!(matches!(
        ensure_full_msr_count(2, 3),
        Err(BackendError::Internal(_))
    ));

    let entries = [
        kvm_msr_entry {
            index: 0x174,
            data: 11,
            ..Default::default()
        },
        kvm_msr_entry {
            index: 0x175,
            data: 22,
            ..Default::default()
        },
        kvm_msr_entry {
            index: 0x176,
            data: 33,
            ..Default::default()
        },
    ];
    let map = saved_msrs(&entries, 3, 3).unwrap();
    assert_eq!(map, BTreeMap::from([(0x174, 11), (0x175, 22), (0x176, 33)]));
    assert!(saved_msrs(&entries, 2, 3).is_err());
}

#[test]
fn validate_restore_shape_keys_and_xsave_len() {
    let filter = MsrFilter {
        allow_inkernel: vec![MsrRange {
            base: 0x174,
            count: 3,
        }],
    };
    let mut good = VcpuState {
        msrs: BTreeMap::from([(0x174, 0), (0x175, 0), (0x176, 0)]),
        xsave: vec![0u8; 4096],
        ..Default::default()
    };
    assert!(validate_restore_shape(&good, Some(&filter), 4096).is_ok());

    // missing key
    good.msrs.remove(&0x175);
    assert!(matches!(
        validate_restore_shape(&good, Some(&filter), 4096),
        Err(BackendError::InvalidState)
    ));
    good.msrs.insert(0x175, 0);

    // extra key
    good.msrs.insert(0x200, 0);
    assert!(validate_restore_shape(&good, Some(&filter), 4096).is_err());
    good.msrs.remove(&0x200);

    // wrong xsave length
    assert!(matches!(
        validate_restore_shape(&good, Some(&filter), 8192),
        Err(BackendError::InvalidState)
    ));

    // no filter ⇒ empty MSR set required
    let empty = VcpuState {
        xsave: vec![0u8; 4096],
        ..Default::default()
    };
    assert!(validate_restore_shape(&empty, None, 4096).is_ok());
    assert!(validate_restore_shape(&good, None, 4096).is_err());
}

#[test]
fn kvm_capabilities_are_honestly_false() {
    let c = kvm_capabilities();
    assert_eq!(c.name, "kvm-stock");
    assert!(!c.deterministic_tsc);
    assert!(!c.deterministic_rng);
    assert!(!c.enforces_tsc_deadline_msr);
}

#[test]
fn mp_state_round_trips() {
    assert_eq!(mp_from_kvm(KVM_MP_STATE_HALTED), MpState::Halted);
    assert_eq!(mp_from_kvm(KVM_MP_STATE_RUNNABLE), MpState::Runnable);
    assert_eq!(mp_from_kvm(0x1234), MpState::Runnable); // anything but HALTED
    assert_eq!(mp_to_kvm(MpState::Halted), KVM_MP_STATE_HALTED);
    assert_eq!(mp_to_kvm(MpState::Runnable), KVM_MP_STATE_RUNNABLE);
    assert_eq!(mp_from_kvm(mp_to_kvm(MpState::Halted)), MpState::Halted);
}

// ---------------------------------------------------------------------------
// kvm_bindings <-> VcpuState conversion round-trips (kill field-routing mutants
// in both directions: `from(to(x)) == x` over distinct field values).
// ---------------------------------------------------------------------------

fn distinct_regs() -> VcpuRegs {
    VcpuRegs {
        rax: 1,
        rbx: 2,
        rcx: 3,
        rdx: 4,
        rsi: 5,
        rdi: 6,
        rsp: 7,
        rbp: 8,
        r8: 9,
        r9: 10,
        r10: 11,
        r11: 12,
        r12: 13,
        r13: 14,
        r14: 15,
        r15: 16,
        rip: 17,
        rflags: 18,
    }
}

fn distinct_segment(n: u8) -> Segment {
    Segment {
        base: 0x1000 * u64::from(n) + 1,
        limit: 0x100 * u32::from(n) + 2,
        selector: (u16::from(n) << 3) | 3,
        type_: n | 0x10,
        present: 1,
        dpl: n & 3,
        db: n & 1,
        s: 1,
        l: (n >> 1) & 1,
        g: 1,
        avl: (n >> 2) & 1,
        unusable: 0,
    }
}

fn distinct_sregs() -> VcpuSregs {
    VcpuSregs {
        cs: distinct_segment(1),
        ds: distinct_segment(2),
        es: distinct_segment(3),
        fs: distinct_segment(4),
        gs: distinct_segment(5),
        ss: distinct_segment(6),
        tr: distinct_segment(7),
        ldt: distinct_segment(8),
        gdt: DescriptorTable {
            base: 0xA000,
            limit: 0xAA,
        },
        idt: DescriptorTable {
            base: 0xB000,
            limit: 0xBB,
        },
        cr0: 0x21,
        cr2: 0x22,
        cr3: 0x23,
        cr4: 0x24,
        cr8: 0x25,
        efer: 0x26,
        apic_base: 0xFEE0_0900,
        flags: 1,
        pdptrs: [0x31, 0x32, 0x33, 0x34],
    }
}

fn distinct_events() -> VcpuEvents {
    VcpuEvents {
        exception_injected: 1,
        exception_nr: 13,
        exception_has_error_code: 1,
        exception_pending: 1,
        exception_error_code: 0xABCD,
        exception_has_payload: 1,
        exception_payload: 0xCAFE_F00D,
        interrupt_injected: 1,
        interrupt_nr: 0x20,
        interrupt_soft: 1,
        interrupt_shadow: 1,
        nmi_injected: 1,
        nmi_pending: 1,
        nmi_masked: 1,
        sipi_vector: 0x99,
        flags: 0x55,
        smi_smm: 1,
        smi_pending: 1,
        smi_inside_nmi: 1,
        smi_latched_init: 1,
        triple_fault_pending: 1,
    }
}

#[test]
fn regs_sregs_events_round_trip() {
    let r = distinct_regs();
    assert_eq!(from_kvm_regs(&to_kvm_regs(&r)), r);

    let s = distinct_sregs();
    assert_eq!(from_kvm_sregs2(&to_kvm_sregs2(&s)), s);

    let e = distinct_events();
    assert_eq!(from_kvm_events(&to_kvm_events(&e)), e);

    let d = DebugRegs {
        db: [1, 2, 3, 4],
        dr6: 5,
        dr7: 6,
        flags: 7,
    };
    assert_eq!(from_kvm_debugregs(&to_kvm_debugregs(&d)), d);
}

#[test]
fn xcr0_round_trips_and_defaults_to_zero() {
    assert_eq!(xcr0_of(&xcrs_of(0x7)), 0x7);
    // A kvm_xcrs with no xcr==0 entry within nr_xcrs reads as 0.
    let empty = kvm_bindings::kvm_xcrs::default();
    assert_eq!(xcr0_of(&empty), 0);
}

#[test]
fn xsave_bytes_round_trip_and_length_check() {
    let mut x = kvm_bindings::kvm_xsave::default();
    x.region[0] = 0xDEAD_BEEF;
    x.region[1023] = 0x0BAD_F00D;
    let bytes = xsave_to_bytes(&x);
    assert_eq!(bytes.len(), 4096);
    let back = xsave_from_bytes(&bytes).unwrap();
    assert_eq!(back.region[0], 0xDEAD_BEEF);
    assert_eq!(back.region[1023], 0x0BAD_F00D);

    // wrong-sized image → InvalidState, never a panic.
    assert!(matches!(
        xsave_from_bytes(&[0u8; 100]),
        Err(BackendError::InvalidState)
    ));
}

// ---------------------------------------------------------------------------
// KVM_EXIT_DETERMINISM decode / complete (the patched-backend surface). Driven
// by a synthetic `kvm_run` whose determinism payload is written by raw offset,
// exactly as the patched kernel would — so the box CI (`nextest`) and Miri
// exercise the decode + completion with no `/dev/kvm`.
// ---------------------------------------------------------------------------

impl SynRun {
    fn set_u32(&self, off: usize, v: u32) {
        for (i, b) in v.to_le_bytes().iter().enumerate() {
            self.set_byte(off + i, *b);
        }
    }
    fn u64(&self, off: usize) -> u64 {
        let mut b = [0u8; 8];
        for (i, slot) in b.iter_mut().enumerate() {
            *slot = self.byte(off + i);
        }
        u64::from_le_bytes(b)
    }
}

/// Stage a determinism exit with the given `insn` kind and result `width`.
fn det_run(insn: u32, width: u32) -> SynRun {
    let s = SynRun::new();
    set_reason(&s, KVM_EXIT_DETERMINISM);
    s.set_u32(DET_INSN, insn);
    s.set_u32(DET_WIDTH, width);
    s
}

#[test]
fn decode_determinism_maps_each_insn() {
    // RDTSC / RDTSCP: 64-bit EDX:EAX, no width surfaced; RDTSCP carries aux.
    let (exit, pending) = decode_exit(det_run(KVM_DETERMINISM_RDTSC, 8).page())
        .unwrap()
        .unwrap();
    assert_eq!(exit, Exit::Rdtsc);
    assert_eq!(
        pending,
        Pending::Determinism {
            rdtscp: false,
            rng: false
        }
    );

    let (exit, pending) = decode_exit(det_run(KVM_DETERMINISM_RDTSCP, 8).page())
        .unwrap()
        .unwrap();
    assert_eq!(exit, Exit::Rdtscp);
    assert_eq!(
        pending,
        Pending::Determinism {
            rdtscp: true,
            rng: false
        }
    );

    // RDRAND / RDSEED: the destination width (2/4/8) is surfaced to the VMM.
    let (exit, pending) = decode_exit(det_run(KVM_DETERMINISM_RDRAND, 4).page())
        .unwrap()
        .unwrap();
    assert_eq!(exit, Exit::Rdrand { width: 4 });
    assert_eq!(
        pending,
        Pending::Determinism {
            rdtscp: false,
            rng: true
        }
    );

    let (exit, pending) = decode_exit(det_run(KVM_DETERMINISM_RDSEED, 2).page())
        .unwrap()
        .unwrap();
    assert_eq!(exit, Exit::Rdseed { width: 2 });
    assert_eq!(
        pending,
        Pending::Determinism {
            rdtscp: false,
            rng: true
        }
    );
}

#[test]
fn decode_determinism_unknown_insn_fails_closed() {
    let err = decode_exit(det_run(99, 8).page()).unwrap_err();
    assert!(matches!(err, BackendError::Internal(_)));
}

#[test]
fn complete_determinism_tsc_writes_value_only() {
    let s = det_run(KVM_DETERMINISM_RDTSC, 8);
    let (_exit, pending) = decode_exit(s.page()).unwrap().unwrap();
    apply_complete_determinism(s.page(), pending, 0x1122_3344_5566_7788, 0).unwrap();
    assert_eq!(s.u64(DET_VALUE), 0x1122_3344_5566_7788);
    // No CF flag for a TSC completion (CF is RNG-only).
    assert_eq!(s.byte(DET_FLAGS), 0);
}

#[test]
fn complete_determinism_rdtscp_writes_value_and_aux() {
    let s = det_run(KVM_DETERMINISM_RDTSCP, 8);
    let (_exit, pending) = decode_exit(s.page()).unwrap().unwrap();
    apply_complete_determinism(s.page(), pending, 0xDEAD_BEEF, 0x00C0_FFEE).unwrap();
    assert_eq!(s.u64(DET_VALUE), 0xDEAD_BEEF);
    assert_eq!(s.u64(DET_AUX), 0x00C0_FFEE); // IA32_TSC_AUX → ECX
    assert_eq!(s.byte(DET_FLAGS), 0);
}

#[test]
fn complete_determinism_rng_writes_value_and_sets_cf() {
    let s = det_run(KVM_DETERMINISM_RDRAND, 8);
    let (_exit, pending) = decode_exit(s.page()).unwrap().unwrap();
    // aux is irrelevant for RNG (no rdtscp); it must NOT be written.
    apply_complete_determinism(s.page(), pending, 0x0102_0304_0506_0708, 0xBAD).unwrap();
    assert_eq!(s.u64(DET_VALUE), 0x0102_0304_0506_0708);
    assert_eq!(s.byte(DET_FLAGS), KVM_DETERMINISM_FLAG_CF);
    assert_eq!(s.u64(DET_AUX), 0); // aux untouched for an RNG draw
}

#[test]
fn complete_determinism_without_pending_is_no_pending_read() {
    let s = det_run(KVM_DETERMINISM_RDTSC, 8);
    assert!(matches!(
        apply_complete_determinism(s.page(), Pending::None, 1, 0),
        Err(BackendError::NoPendingRead)
    ));
    // A non-determinism read-style completion must not satisfy a determinism
    // pending either: apply_complete_read rejects it.
    let (_exit, pending) = decode_exit(s.page()).unwrap().unwrap();
    assert!(matches!(
        apply_complete_read(s.page(), pending, 1),
        Err(BackendError::NoPendingRead)
    ));
}

// ---------------------------------------------------------------------------
// Interrupt-injection planning (the userspace-irqchip handshake, task 32),
// driven by a synthetic `kvm_run` whose `ready_for_interrupt_injection` /
// `request_interrupt_window` are plain top-level fields written/read directly —
// so the box CI (`nextest`) and Miri exercise the ready/not-ready branch, the
// KVM_INTERRUPT queue decision, and the interrupt-window arm/clear with no
// `/dev/kvm`.
// ---------------------------------------------------------------------------

impl SynRun {
    /// Set `kvm_run.ready_for_interrupt_injection` (kernel → user).
    fn set_ready(&self, ready: bool) {
        // SAFETY: plain top-level field of the owned, zeroed `kvm_run`.
        unsafe { (*self.run()).ready_for_interrupt_injection = u8::from(ready) };
    }
    /// Read `kvm_run.request_interrupt_window` (user → kernel) back.
    fn request_window(&self) -> u8 {
        // SAFETY: plain top-level field of the owned `kvm_run`.
        unsafe { (*self.run()).request_interrupt_window }
    }
    /// Pre-set `kvm_run.request_interrupt_window` (to prove `plan_irq_entry`
    /// clears a stale request).
    fn set_request_window(&self, on: bool) {
        // SAFETY: plain top-level field of the owned `kvm_run`.
        unsafe { (*self.run()).request_interrupt_window = u8::from(on) };
    }
}

#[test]
fn plan_irq_entry_queues_when_ready() {
    // A pending vector + the guest ready ⇒ queue it now, with no window request.
    let s = SynRun::new();
    s.set_ready(true);
    s.set_request_window(true); // a stale request that must be cleared
    assert_eq!(plan_irq_entry(s.page(), Some(0x40)), IrqEntry::Queue(0x40));
    assert_eq!(s.request_window(), 0, "window request cleared when queuing");
}

#[test]
fn plan_irq_entry_requests_window_when_not_ready() {
    // A pending vector + the guest NOT ready ⇒ arm the interrupt window and run
    // (the vector stays pending; the caller retries on KVM_EXIT_IRQ_WINDOW_OPEN).
    let s = SynRun::new();
    s.set_ready(false);
    assert_eq!(plan_irq_entry(s.page(), Some(0x40)), IrqEntry::Run);
    assert_eq!(s.request_window(), 1, "window armed when not injectable");
}

#[test]
fn plan_irq_entry_clears_window_when_nothing_pending() {
    // No pending vector ⇒ run directly, and any stale window request is cleared
    // (so a one-shot window from a prior delivery never lingers).
    let s = SynRun::new();
    s.set_ready(true);
    s.set_request_window(true);
    assert_eq!(plan_irq_entry(s.page(), None), IrqEntry::Run);
    assert_eq!(s.request_window(), 0, "stale window request cleared");

    // Even when the guest is not ready, no pending vector ⇒ no window request.
    let s = SynRun::new();
    s.set_ready(false);
    s.set_request_window(true);
    assert_eq!(plan_irq_entry(s.page(), None), IrqEntry::Run);
    assert_eq!(s.request_window(), 0);
}

#[test]
fn interrupt_fields_round_trip_through_vcpu_events() {
    // The injection state KVM threads across a save/restore (the entry-interrupt
    // vector + the STI/MOV-SS shadow) must survive `to_kvm_events(from_kvm_events)`
    // so snapshot/replay re-injects an in-flight vector exactly. Pin the interrupt
    // fields specifically (the broader round-trip is covered by
    // `regs_sregs_events_round_trip`).
    let e = VcpuEvents {
        interrupt_injected: 1,
        interrupt_nr: 0x40,
        interrupt_soft: 0,
        interrupt_shadow: 1,
        ..Default::default()
    };
    let k = to_kvm_events(&e);
    assert_eq!(k.interrupt.injected, 1);
    assert_eq!(k.interrupt.nr, 0x40);
    assert_eq!(k.interrupt.shadow, 1);
    let back = from_kvm_events(&k);
    assert_eq!(back.interrupt_injected, 1);
    assert_eq!(back.interrupt_nr, 0x40);
    assert_eq!(back.interrupt_shadow, 1);
    assert_eq!(back, e);
}
