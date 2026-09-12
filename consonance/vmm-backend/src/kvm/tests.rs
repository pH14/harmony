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
use crate::arch::x86::{CpuidEntry, CpuidModel, MsrFilter, MsrRange};
use crate::arch::x86::{DebugRegs, DescriptorTable, Segment, VcpuEvents, VcpuRegs, VcpuSregs};
use crate::exit::Exit;
use crate::types::Gpa;
use crate::types::MpState;

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
        io.direction = 1;
        io.size = 1;
        io.port = 0x3F8;
        io.count = 1;
        io.data_offset = PIO_OFF as u64;
    }
    s.set_byte(PIO_OFF, 0x42);

    let (exit, pending) = decode_exit(s.page()).unwrap().unwrap();
    assert_eq!(
        exit,
        Exit::Arch(X86Exit::Io {
            port: 0x3F8,
            size: 1,
            write: Some(0x42)
        })
    );
    assert_eq!(pending, Pending::None);
    assert!(decoded_exit_stages_completion(&exit, pending));
}

#[test]
fn decode_io_in_arms_pending() {
    let s = SynRun::new();
    set_reason(&s, KVM_EXIT_IO);
    // SAFETY: union sub-field writes.
    unsafe {
        let io = &mut (*s.run()).__bindgen_anon_1.io;
        io.direction = 0;
        io.size = 2;
        io.port = 0x60;
        io.count = 1;
        io.data_offset = PIO_OFF as u64;
    }
    let (exit, pending) = decode_exit(s.page()).unwrap().unwrap();
    assert_eq!(
        exit,
        Exit::Arch(X86Exit::Io {
            port: 0x60,
            size: 2,
            write: None
        })
    );
    assert_eq!(
        pending,
        Pending::IoIn {
            data_offset: PIO_OFF as u64,
            size: 2
        }
    );
    assert!(!decoded_exit_stages_completion(&exit, pending));
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
        io.count = 7;
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
        io.data_offset = (s.len as u64) - 2;
    }
    assert!(matches!(
        decode_exit(s.page()),
        Err(BackendError::Memory(_))
    ));
}

#[test]
fn decode_mmio_store_and_load() {
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
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(0xFEE0_0000),
            size: 4,
            write: Some(0x1234_5678)
        })
    );
    assert_eq!(pending, Pending::None);
    assert!(decoded_exit_stages_completion(&exit, pending));

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
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(0xFEE0_0080),
            size: 4,
            write: None
        })
    );
    assert_eq!(pending, Pending::MmioLoad { len: 4 });
    assert!(!decoded_exit_stages_completion(&exit, pending));
}

#[test]
fn decode_rdmsr_and_wrmsr() {
    let s = SynRun::new();
    set_reason(&s, KVM_EXIT_X86_RDMSR);
    // SAFETY: union sub-field writes.
    unsafe { (*s.run()).__bindgen_anon_1.msr.index = 0x1B };
    let (exit, pending) = decode_exit(s.page()).unwrap().unwrap();
    assert_eq!(exit, Exit::Arch(X86Exit::Rdmsr { index: 0x1B }));
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
        Exit::Arch(X86Exit::Wrmsr {
            index: 0x6E0,
            value: 0xDEAD_BEEF
        })
    );
    assert_eq!(pending, Pending::Wrmsr);
}

#[test]
fn decode_terminal_and_control_exits() {
    for (reason, want) in [
        (KVM_EXIT_HLT, Exit::Common(CommonExit::Idle)),
        (KVM_EXIT_SHUTDOWN, Exit::Common(CommonExit::Shutdown)),
    ] {
        let s = SynRun::new();
        set_reason(&s, reason);
        let (exit, pending) = decode_exit(s.page()).unwrap().unwrap();
        assert_eq!(exit, want);
        assert_eq!(pending, Pending::None);
    }
    let s = SynRun::new();
    set_reason(&s, KVM_EXIT_IRQ_WINDOW_OPEN);
    assert_eq!(decode_exit(s.page()).unwrap(), None);
}

#[test]
fn decode_error_and_unknown_exits_fail_closed() {
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

    let s = SynRun::new();
    apply_complete_read(s.page(), Pending::MmioLoad { len: 4 }, 0xAABB_CCDD).unwrap();
    // SAFETY: read back the union member just written.
    let data = unsafe { (*s.run()).__bindgen_anon_1.mmio.data };
    assert_eq!(&data[..4], &0xAABB_CCDDu32.to_le_bytes());

    let s = SynRun::new();
    apply_complete_read(s.page(), Pending::Rdmsr, 0x1234).unwrap();
    // SAFETY: read back the union member just written.
    let msr = unsafe { (*s.run()).__bindgen_anon_1.msr };
    assert_eq!(msr.data, 0x1234);
    assert_eq!(msr.error, 0);

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
    for p in [Pending::Rdmsr, Pending::Wrmsr] {
        let s = SynRun::new();
        apply_complete_fault(s.page(), p).unwrap();
        // SAFETY: read back the union member.
        assert_eq!(unsafe { (*s.run()).__bindgen_anon_1.msr.error }, 1);
    }
    let s = SynRun::new();
    // SAFETY: union sub-field write.
    unsafe { (*s.run()).__bindgen_anon_1.msr.error = 9 };
    apply_complete_ok(s.page(), Pending::Wrmsr).unwrap();
    assert_eq!(unsafe { (*s.run()).__bindgen_anon_1.msr.error }, 0);

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

#[test]
fn retire_staged_completion_is_a_noop_without_a_stage() {
    let s = SynRun::new();
    let mut pending = Pending::None;
    let mut staged = false;
    let mut entries = 0;
    retire_staged_completion(s.page(), &mut pending, &mut staged, || {
        entries += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(entries, 0, "a no-pending retirement must not enter KVM");
    assert!(!staged);
    assert_eq!(pending, Pending::None);
    assert_eq!(s.page().immediate_exit(), 0);
}

#[test]
fn retire_staged_completion_uses_one_immediate_entry_and_preserves_next_exit() {
    let s = SynRun::new();
    let mut pending = Pending::IoIn {
        data_offset: PIO_OFF as u64,
        size: 1,
    };
    let mut staged = true;
    let mut entries = 0;
    let mut next_scripted_exit = Some(KVM_EXIT_HLT);
    retire_staged_completion(s.page(), &mut pending, &mut staged, || {
        entries += 1;
        assert_eq!(s.page().immediate_exit(), 1);
        assert_eq!(next_scripted_exit, Some(KVM_EXIT_HLT));
        Err(std::io::Error::from_raw_os_error(libc::EINTR))
    })
    .unwrap();
    assert_eq!(entries, 1);
    assert_eq!(next_scripted_exit.take(), Some(KVM_EXIT_HLT));
    assert!(!staged);
    assert_eq!(pending, Pending::None);
    assert_eq!(s.page().immediate_exit(), 0);
}

#[test]
fn retire_staged_completion_error_clears_one_shot_and_keeps_stage() {
    let s = SynRun::new();
    let mut pending = Pending::None;
    let mut staged = true;
    let error = retire_staged_completion(s.page(), &mut pending, &mut staged, || {
        assert_eq!(s.page().immediate_exit(), 1);
        Err(std::io::Error::from_raw_os_error(libc::EIO))
    })
    .expect_err("a non-EINTR completion entry must fail closed");
    assert!(matches!(error, BackendError::Io(_)));
    assert_eq!(s.page().immediate_exit(), 0);
    assert!(
        staged,
        "an uncertain completion remains conservatively staged"
    );
    assert_eq!(pending, Pending::None);
}

#[test]
fn retire_staged_write_completion_without_pending_uses_immediate_entry() {
    let s = SynRun::new();
    let mut pending = Pending::None;
    let mut staged = true;
    let mut entries = 0;
    retire_staged_completion(s.page(), &mut pending, &mut staged, || {
        entries += 1;
        assert_eq!(s.page().immediate_exit(), 1);
        Err(std::io::Error::from_raw_os_error(libc::EINTR))
    })
    .unwrap();
    assert_eq!(entries, 1);
    assert!(!staged);
    assert_eq!(pending, Pending::None);
    assert_eq!(s.page().immediate_exit(), 0);
}

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

    good.msrs.remove(&0x175);
    assert!(matches!(
        validate_restore_shape(&good, Some(&filter), 4096),
        Err(BackendError::InvalidState)
    ));
    good.msrs.insert(0x175, 0);

    good.msrs.insert(0x200, 0);
    assert!(validate_restore_shape(&good, Some(&filter), 4096).is_err());
    good.msrs.remove(&0x200);

    assert!(matches!(
        validate_restore_shape(&good, Some(&filter), 8192),
        Err(BackendError::InvalidState)
    ));

    let empty = VcpuState {
        xsave: vec![0u8; 4096],
        ..Default::default()
    };
    assert!(validate_restore_shape(&empty, None, 4096).is_ok());
    assert!(validate_restore_shape(&good, None, 4096).is_err());
}

#[test]
fn kvm_capabilities_report_the_stock_backend_name() {
    let c = kvm_capabilities();
    assert_eq!(c.name, "kvm-stock");
}

#[test]
fn mp_state_round_trips() {
    assert_eq!(mp_from_kvm(KVM_MP_STATE_HALTED), MpState::Halted);
    assert_eq!(mp_from_kvm(KVM_MP_STATE_RUNNABLE), MpState::Runnable);
    assert_eq!(mp_from_kvm(0x1234), MpState::Runnable);
    assert_eq!(mp_to_kvm(MpState::Halted), KVM_MP_STATE_HALTED);
    assert_eq!(mp_to_kvm(MpState::Runnable), KVM_MP_STATE_RUNNABLE);
    assert_eq!(mp_from_kvm(mp_to_kvm(MpState::Halted)), MpState::Halted);
}

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

    assert!(matches!(
        xsave_from_bytes(&[0u8; 100]),
        Err(BackendError::InvalidState)
    ));
}

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
    let s = SynRun::new();
    s.set_ready(true);
    s.set_request_window(true);
    assert_eq!(
        plan_irq_entry(s.page(), Some(0x40), true),
        IrqEntry::Queue(0x40)
    );
    assert_eq!(s.request_window(), 0, "window request cleared when queuing");
}

#[test]
fn plan_irq_entry_requests_window_when_readiness_is_stale() {
    let s = SynRun::new();
    s.set_ready(true);
    assert_eq!(plan_irq_entry(s.page(), Some(0x40), false), IrqEntry::Run);
    assert_eq!(s.request_window(), 1, "window armed until the next exit");
    assert_eq!(plan_irq_entry(s.page(), None, false), IrqEntry::Run);
    assert_eq!(s.request_window(), 0);
}

#[test]
fn plan_irq_entry_requests_window_when_not_ready() {
    let s = SynRun::new();
    s.set_ready(false);
    assert_eq!(plan_irq_entry(s.page(), Some(0x40), true), IrqEntry::Run);
    assert_eq!(s.request_window(), 1, "window armed when not injectable");
}

#[test]
fn plan_irq_entry_clears_window_when_nothing_pending() {
    let s = SynRun::new();
    s.set_ready(true);
    s.set_request_window(true);
    assert_eq!(plan_irq_entry(s.page(), None, true), IrqEntry::Run);
    assert_eq!(s.request_window(), 0, "stale window request cleared");

    let s = SynRun::new();
    s.set_ready(false);
    s.set_request_window(true);
    assert_eq!(plan_irq_entry(s.page(), None, true), IrqEntry::Run);
    assert_eq!(s.request_window(), 0);
}

#[test]
fn interrupt_fields_round_trip_through_vcpu_events() {
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

#[test]
fn restore_sregs_invalidates_translations_and_installs_exact_snapshot() {
    for cr0 in [0x21, 0x10021, 0x80000021, 0x80010021] {
        let saved = VcpuSregs {
            cr0,
            cr3: 0x1000,
            cr4: 0x20,
            ..VcpuSregs::default()
        };
        let mut writes = Vec::new();
        restore_sregs2_with_flush(&saved, |sregs| {
            writes.push(from_kvm_sregs2(sregs));
            Ok(())
        })
        .unwrap();
        assert_eq!(writes.len(), 2);
        assert_ne!(writes[0].cr0, writes[1].cr0);
        assert_eq!(writes[0].cr0 ^ writes[1].cr0, 0x10000);
        let mut transient = writes[0];
        transient.cr0 = saved.cr0;
        assert_eq!(
            transient, saved,
            "only write protection changes temporarily"
        );
        assert_eq!(writes[1], saved, "the final state is the exact snapshot");
    }
}

#[test]
fn restore_sregs_stops_at_either_failed_write() {
    for failure in [1, 2] {
        let mut calls = 0;
        let result = restore_sregs2_with_flush(&VcpuSregs::default(), |_| {
            calls += 1;
            if calls == failure {
                Err(BackendError::Internal("rejected special registers"))
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert_eq!(calls, failure);
    }
}
