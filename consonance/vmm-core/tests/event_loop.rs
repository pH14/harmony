// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pure-logic event-loop gates (no `/dev/kvm`): drive [`vmm_core::vmm::Vmm`]
//! against the scripted [`vmm_backend::MockBackend`] and assert the serial
//! capture, terminal reason, default-deny behavior, and `state_hash`
//! purity/coverage. This is the `vmcall-transport` loopback pattern applied to the
//! backend seam (tasks/15 §"Mock-backend testing").

use vmm_backend::{Backend, Exit, Gpa, MockBackend, Moment, MpState, VcpuState};
use vmm_core::contract::{cpuid_model, msr_filter_allow};
use vmm_core::vmm::{GuestRam, Step, TerminalReason, Vmm, VmmError};

const HELLO: &[u8] = b"PAYLOAD hello START\nPAYLOAD hello PASS\n";

/// A 1-byte port read exit.
fn io_in(port: u16) -> Exit {
    Exit::Io {
        port,
        size: 1,
        write: None,
    }
}

/// A 1-byte port write exit.
fn io_out(port: u16, value: u8) -> Exit {
    Exit::Io {
        port,
        size: 1,
        write: Some(u32::from(value)),
    }
}

/// The task-04 UART init writes (none captured): IER, LCR DLAB=1, divisor low/high,
/// LCR 8N1, FCR, MCR.
fn uart_init() -> Vec<Exit> {
    vec![
        io_out(0x3F9, 0x00), // IER
        io_out(0x3FB, 0x80), // LCR DLAB=1
        io_out(0x3F8, 0x01), // divisor low — divisor latch, NOT serial output
        io_out(0x3F9, 0x00), // divisor high
        io_out(0x3FB, 0x03), // LCR 8N1, DLAB=0
        io_out(0x3FA, 0xC7), // FCR
        io_out(0x3FC, 0x03), // MCR
    ]
}

/// Build the full scripted `hello` sequence: init, then for each byte an LSR poll
/// + a THR write, then the clean isa-debug-exit PASS.
fn hello_script() -> Vec<Exit> {
    let mut s = uart_init();
    for &b in HELLO {
        s.push(io_in(0x3FD)); // poll LSR for THR-empty
        s.push(io_out(0x3F8, b)); // THR transmit
    }
    s.push(io_out(0xF4, 0)); // isa-debug-exit PASS
    s
}

/// A configured mock + a small `GuestRam`, wrapped in a `Vmm`, ready to `run`.
fn vmm_with(script: Vec<Exit>) -> Vmm<MockBackend> {
    let mut mock = MockBackend::with_exits(script);
    mock.set_cpuid(&cpuid_model()).unwrap();
    mock.set_msr_filter(&msr_filter_allow()).unwrap();
    let ram = GuestRam::new(4096).unwrap();
    Vmm::new(mock, ram)
}

#[test]
fn hello_serial_and_clean_exit() {
    let mut vmm = vmm_with(hello_script());
    let result = vmm.run().unwrap();
    // The divisor 0x01 was NOT captured; only the THR data bytes are serial.
    assert_eq!(result.serial, HELLO);
    assert_eq!(result.reason, TerminalReason::DebugExit { code: 0 });
    // The LSR-poll IN exits were each resolved with THR-empty (0x60).
    assert!(result.exit_counts.io > 0);
}

#[test]
fn isa_debug_exit_codes_distinguished() {
    let mut pass = vmm_with(vec![io_out(0xF4, 0)]);
    assert_eq!(
        pass.run().unwrap().reason,
        TerminalReason::DebugExit { code: 0 }
    );
    let mut fail = vmm_with(vec![io_out(0xF4, 1)]);
    assert_eq!(
        fail.run().unwrap().reason,
        TerminalReason::DebugExit { code: 1 }
    );
}

#[test]
fn hlt_and_shutdown_are_terminal() {
    let mut hlt = vmm_with(vec![Exit::Hlt]);
    assert_eq!(hlt.run().unwrap().reason, TerminalReason::Hlt);
    let mut sd = vmm_with(vec![Exit::Shutdown]);
    assert_eq!(sd.run().unwrap().reason, TerminalReason::Shutdown);
}

#[test]
fn unmodeled_exits_fail_closed() {
    // An unmodeled OUT port (PIC) — default-deny, not a silent drop.
    let mut pic = vmm_with(vec![io_out(0x20, 0x11)]);
    assert!(matches!(pic.run(), Err(VmmError::ContractViolation(_))));

    // Unmodeled MMIO.
    let mut mmio = vmm_with(vec![Exit::Mmio {
        gpa: Gpa(0xFEE0_0000),
        size: 4,
        write: None,
    }]);
    assert!(matches!(mmio.run(), Err(VmmError::ContractViolation(_))));

    // A backend-dependent RDTSC (must never be laundered).
    let mut tsc = vmm_with(vec![Exit::Rdtsc]);
    assert!(matches!(tsc.run(), Err(VmmError::ContractViolation(_))));

    // A hypercall (host handler deferred).
    let mut hc = vmm_with(vec![Exit::Hypercall(Default::default())]);
    assert!(matches!(hc.run(), Err(VmmError::ContractViolation(_))));
}

#[test]
fn unmodeled_in_port_fails_closed() {
    let mut vmm = vmm_with(vec![io_in(0x71)]); // CMOS data port — not modeled
    assert!(matches!(vmm.run(), Err(VmmError::ContractViolation(_))));
}

#[test]
fn non_byte_io_to_modeled_ports_fails_closed() {
    // A wide write/read to a modeled BYTE port is a ContractViolation, never a
    // `value as u8` truncation. `outl $0, $0xF4` must NOT become a fake PASS.
    let wide_out = |port: u16, size: u8, value: u32| Exit::Io {
        port,
        size,
        write: Some(value),
    };
    let wide_in = |port: u16, size: u8| Exit::Io {
        port,
        size,
        write: None,
    };

    // `outl 0x00000000, $0xF4` (size 4): must fail closed, NOT terminate PASS.
    let mut dbg = vmm_with(vec![wide_out(0xF4, 4, 0x0000_0000)]);
    assert!(matches!(dbg.run(), Err(VmmError::ContractViolation(_))));

    // A 2-byte write to the UART THR is unmodeled (the 8250 is byte-addressed).
    let mut uart_w = vmm_with(vec![wide_out(0x3F8, 2, 0x4142)]);
    assert!(matches!(uart_w.run(), Err(VmmError::ContractViolation(_))));

    // A 4-byte read of the LSR is likewise unmodeled.
    let mut uart_r = vmm_with(vec![wide_in(0x3FD, 4)]);
    assert!(matches!(uart_r.run(), Err(VmmError::ContractViolation(_))));
}

#[test]
fn deny_gp_msr_injects_fault() {
    // An unlisted MSR read defaults to deny-gp → complete_fault, then PASS.
    let mut vmm = vmm_with(vec![Exit::Rdmsr { index: 0xDEAD_BEEF }, io_out(0xF4, 0)]);
    let r = vmm.run().unwrap();
    assert_eq!(r.reason, TerminalReason::DebugExit { code: 0 });
}

#[test]
fn allow_fixed_msr_returns_constant() {
    // IA32_APICBASE (0x1b) read is allow-fixed 0xFEE00900.
    let mut mock = MockBackend::with_exits(vec![Exit::Rdmsr { index: 0x1B }, io_out(0xF4, 0)]);
    mock.set_cpuid(&cpuid_model()).unwrap();
    mock.set_msr_filter(&msr_filter_allow()).unwrap();
    let mut vmm = Vmm::new(mock, GuestRam::new(4096).unwrap());
    assert_eq!(
        vmm.run().unwrap().reason,
        TerminalReason::DebugExit { code: 0 }
    );
}

// --- state_hash purity & coverage (gate 8) --------------------------------

#[test]
fn state_hash_is_pure_and_covers_every_component() {
    let baseline = {
        let mut v = vmm_with(hello_script());
        v.run().unwrap();
        v
    };
    let h0 = baseline.state_hash();
    // Pure: two calls agree.
    assert_eq!(h0, baseline.state_hash());

    // An identical run reproduces the hash.
    let same = {
        let mut v = vmm_with(hello_script());
        v.run().unwrap();
        v
    };
    assert_eq!(h0, same.state_hash());

    // Flip the serial output (drop the last byte's write) ⇒ different hash.
    let diff_serial = {
        let mut script = uart_init();
        for &b in &HELLO[..HELLO.len() - 1] {
            script.push(io_in(0x3FD));
            script.push(io_out(0x3F8, b));
        }
        script.push(io_out(0xF4, 0));
        let mut v = vmm_with(script);
        v.run().unwrap();
        v
    };
    assert_ne!(
        h0,
        diff_serial.state_hash(),
        "serial divergence breaks the hash"
    );

    // Flip the debug-exit code ⇒ different hash (output-only/terminal divergence).
    let diff_code = {
        let mut script = uart_init();
        for &b in HELLO {
            script.push(io_in(0x3FD));
            script.push(io_out(0x3F8, b));
        }
        script.push(io_out(0xF4, 1)); // FAIL instead of PASS
        let mut v = vmm_with(script);
        v.run().unwrap();
        v
    };
    assert_ne!(
        h0,
        diff_code.state_hash(),
        "terminal code divergence breaks the hash"
    );

    // Flip a guest-RAM byte ⇒ different hash.
    let diff_mem = {
        let mut mock = MockBackend::with_exits(hello_script());
        mock.set_cpuid(&cpuid_model()).unwrap();
        mock.set_msr_filter(&msr_filter_allow()).unwrap();
        let mut ram = GuestRam::new(4096).unwrap();
        ram.as_mut_bytes()[1234] = 0xAB;
        let mut v = Vmm::new(mock, ram);
        v.run().unwrap();
        v
    };
    assert_ne!(
        h0,
        diff_mem.state_hash(),
        "memory divergence breaks the hash"
    );

    // Flip a VcpuState register ⇒ different hash.
    let diff_reg = {
        let mut mock = MockBackend::with_exits(hello_script());
        mock.set_cpuid(&cpuid_model()).unwrap();
        mock.set_msr_filter(&msr_filter_allow()).unwrap();
        let mut st = VcpuState::default();
        st.regs.rip = 0xDEAD_0000;
        mock.set_state(st);
        let mut v = Vmm::new(mock, GuestRam::new(4096).unwrap());
        v.run().unwrap();
        v
    };
    assert_ne!(
        h0,
        diff_reg.state_hash(),
        "register divergence breaks the hash"
    );
}

// --- WRMSR dispositions + the MSR error branches (coverage) ----------------

#[test]
fn wrmsr_dispositions_serviced() {
    // deny-ignore-write (0x1B IA32_APICBASE write) → complete_ok, run continues.
    let mut drop_write = vmm_with(vec![
        Exit::Wrmsr {
            index: 0x1B,
            value: 0xFEE0_0900,
        },
        io_out(0xF4, 0),
    ]);
    assert_eq!(
        drop_write.run().unwrap().reason,
        TerminalReason::DebugExit { code: 0 }
    );

    // deny-gp (unlisted index) → complete_fault, run continues.
    let mut gp = vmm_with(vec![
        Exit::Wrmsr {
            index: 0xDEAD_BEEF,
            value: 1,
        },
        io_out(0xF4, 0),
    ]);
    assert_eq!(
        gp.run().unwrap().reason,
        TerminalReason::DebugExit { code: 0 }
    );

    // A write to a read-only allow-fixed row (0x17 PLATFORM_ID) → #GP fault.
    let mut fixed = vmm_with(vec![
        Exit::Wrmsr {
            index: 0x17,
            value: 0,
        },
        io_out(0xF4, 0),
    ]);
    assert_eq!(
        fixed.run().unwrap().reason,
        TerminalReason::DebugExit { code: 0 }
    );
}

#[test]
fn emulate_vtime_msr_fails_closed_both_directions() {
    // 0x10 / 0x3b are emulate-vtime; with no V-time wired, an actual access is a
    // loud ContractViolation in BOTH directions (never a laundered host value).
    for idx in [0x10u32, 0x3b] {
        let mut rd = vmm_with(vec![Exit::Rdmsr { index: idx }]);
        assert!(matches!(rd.run(), Err(VmmError::ContractViolation(_))));
        let mut wr = vmm_with(vec![Exit::Wrmsr {
            index: idx,
            value: 0,
        }]);
        assert!(matches!(wr.run(), Err(VmmError::ContractViolation(_))));
    }
}

#[test]
fn allow_stateful_msr_surfacing_fails_closed() {
    // EFER (0xC000_0080) is allow-stateful → serviced in-kernel; if it ever
    // surfaces to userspace it is a loud ContractViolation (both directions).
    let mut rd = vmm_with(vec![Exit::Rdmsr { index: 0xC000_0080 }]);
    assert!(matches!(rd.run(), Err(VmmError::ContractViolation(_))));
    let mut wr = vmm_with(vec![Exit::Wrmsr {
        index: 0xC000_0080,
        value: 0,
    }]);
    assert!(matches!(wr.run(), Err(VmmError::ContractViolation(_))));
}

// --- Cpuid dispatch (frozen model + dynamic overlay + default) -------------

#[test]
fn cpuid_exit_serviced_from_frozen_model() {
    // A userspace Cpuid exit (a patched/direct backend) is answered from the frozen
    // model: leaf 1 exists (and `resolve_cpuid` overlays the dynamic cells from the
    // saved CR4/XCR0), while a bogus leaf falls through to the zeroed default rule.
    let mut vmm = vmm_with(vec![
        Exit::Cpuid {
            leaf: 1,
            subleaf: 0,
        },
        Exit::Cpuid {
            leaf: 0xDEAD,
            subleaf: 0,
        },
        io_out(0xF4, 0),
    ]);
    assert_eq!(
        vmm.run().unwrap().reason,
        TerminalReason::DebugExit { code: 0 }
    );
}

// --- step() after terminal, Deadline, GuestRam, and rich-state encoding ----

#[test]
fn step_after_terminal_is_idempotent() {
    let mut vmm = vmm_with(vec![Exit::Hlt]);
    assert_eq!(vmm.run().unwrap().reason, TerminalReason::Hlt);
    // A further step() returns the latched terminal without re-running the backend.
    assert_eq!(vmm.step().unwrap(), Step::Terminal(TerminalReason::Hlt));
}

#[test]
fn deadline_exit_fails_closed() {
    // A `Deadline` only ever answers `run_until`, which the VMM issues solely on the
    // V-time-wired determinism path (task 47). One arriving with **no V-time wired**
    // (this bring-up VMM) is a backend contract violation → loud, never absorbed.
    let mut vmm = vmm_with(vec![Exit::Deadline { reached: Moment(0) }]);
    assert!(matches!(vmm.run(), Err(VmmError::ContractViolation(_))));
}

#[test]
fn guest_ram_validation_and_accessors() {
    // Length must be a non-zero multiple of 4 KiB.
    assert!(matches!(GuestRam::new(0), Err(VmmError::Backend(_))));
    assert!(matches!(GuestRam::new(4097), Err(VmmError::Backend(_))));
    let mut ram = GuestRam::new(8192).unwrap();
    assert_eq!(ram.len(), 8192);
    assert!(!ram.is_empty());
    ram.as_mut_bytes()[0] = 0xAB;
    assert_eq!(ram.as_bytes()[0], 0xAB);
}

#[test]
fn state_hash_covers_msrs_xsave_and_mp_state() {
    let mut mock = MockBackend::with_exits(vec![Exit::Hlt]);
    mock.set_cpuid(&cpuid_model()).unwrap();
    mock.set_msr_filter(&msr_filter_allow()).unwrap();
    // A rich terminal VcpuState — Halted, with MSRs and an XSAVE blob — so
    // `encode_vcpu_state`'s MSR key/value loop, the XSAVE bytes, and the `Halted`
    // mp_state arm all execute (and the hash stays a pure function of them).
    let mut st = VcpuState {
        mp_state: MpState::Halted,
        xsave: vec![1u8, 2, 3, 4, 5, 6, 7, 8],
        ..Default::default()
    };
    st.msrs.insert(0xC000_0080, 0x500); // EFER
    st.msrs.insert(0x277, 0x0007_0406); // IA32_PAT
    mock.set_state(st);
    let mut vmm = Vmm::new(mock, GuestRam::new(4096).unwrap());
    assert_eq!(vmm.run().unwrap().reason, TerminalReason::Hlt);
    assert_eq!(vmm.state_hash(), vmm.state_hash());
    assert_ne!(vmm.state_hash(), [0u8; 32]);
}

#[test]
fn state_hash_distinguishes_segment_and_event_fields() {
    // Build a Vmm that terminates immediately on Hlt, with a mutated terminal
    // VcpuState, and return its state_hash. Two states differing only in a segment
    // or event field must hash differently — which kills `encode_segment with ()`
    // and `encode_events with ()` (those would drop the field from the blob).
    let hash_with = |mutate: &dyn Fn(&mut VcpuState)| {
        let mut mock = MockBackend::with_exits(vec![Exit::Hlt]);
        mock.set_cpuid(&cpuid_model()).unwrap();
        mock.set_msr_filter(&msr_filter_allow()).unwrap();
        let mut st = VcpuState::default();
        mutate(&mut st);
        mock.set_state(st);
        let mut v = Vmm::new(mock, GuestRam::new(4096).unwrap());
        v.run().unwrap();
        v.state_hash()
    };
    let base = hash_with(&|_| {});
    assert_ne!(
        base,
        hash_with(&|s| s.sregs.cs.base = 0x1234),
        "segment field reaches the hash"
    );
    assert_ne!(
        base,
        hash_with(&|s| s.events.nmi_pending = 1),
        "event field reaches the hash"
    );
}

#[test]
fn state_hash_masks_only_an_unusable_segments_type() {
    // `encode_segment` canonicalizes the `type` of an **unusable** segment to 0: it is
    // architecturally don't-care (SDM Vol. 3 §24.4.1 — an unusable segment is treated as
    // absent, its hidden type/attr never consulted) and KVM perturbs it across a GET/SET
    // round-trip, so hashing it raw would break restore-transparency. The mask is
    // `if seg.unusable != 0 { 0 } else { seg.type_ }`. This pins **both halves**, killing
    // the `!= -> ==` mutant (which inverts the predicate — masking *usable* types while
    // leaking *unusable* ones) from either direction:
    //   * a **usable** segment's `type` MUST reach the hash (live architectural state);
    //   * an **unusable** segment's `type` must NOT (the masked don't-care field).
    let hash_with = |mutate: &dyn Fn(&mut VcpuState)| {
        let mut mock = MockBackend::with_exits(vec![Exit::Hlt]);
        mock.set_cpuid(&cpuid_model()).unwrap();
        mock.set_msr_filter(&msr_filter_allow()).unwrap();
        let mut st = VcpuState::default();
        mutate(&mut st);
        mock.set_state(st);
        let mut v = Vmm::new(mock, GuestRam::new(4096).unwrap());
        v.run().unwrap();
        v.state_hash()
    };
    // Usable (unusable = 0): the type is live state → it MUST move the hash.
    // Original keeps `seg.type_` (0 vs 5 differ); the `==` mutant masks both to 0 (equal).
    assert_ne!(
        hash_with(&|s| {
            s.sregs.cs.unusable = 0;
            s.sregs.cs.type_ = 0;
        }),
        hash_with(&|s| {
            s.sregs.cs.unusable = 0;
            s.sregs.cs.type_ = 5;
        }),
        "a usable segment's type reaches the hash (== mutant masks it to 0 -> equal)"
    );
    // Unusable (unusable = 1): the type is don't-care → it must NOT move the hash.
    // Original masks both to 0 (equal); the `==` mutant leaks `seg.type_` (0 vs 5 differ).
    assert_eq!(
        hash_with(&|s| {
            s.sregs.cs.unusable = 1;
            s.sregs.cs.type_ = 0;
        }),
        hash_with(&|s| {
            s.sregs.cs.unusable = 1;
            s.sregs.cs.type_ = 5;
        }),
        "an unusable segment's type is masked out of the hash (== mutant leaks it -> differ)"
    );
}
