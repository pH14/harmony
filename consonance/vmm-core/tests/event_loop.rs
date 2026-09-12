// SPDX-License-Identifier: AGPL-3.0-or-later

use vmm_backend::{
    Backend, CommonExit, Exit, Gpa, MockBackend, MpState, VcpuState, X86, X86Exit, X86Policy,
};
use vmm_core::vendor::x86::contract::{cpuid_model, msr_filter_allow};
use vmm_core::vmm::{GuestRam, Step, TerminalReason, Vmm, VmmError};

const HELLO: &[u8] = b"PAYLOAD hello START\nPAYLOAD hello PASS\n";

fn io_in(port: u16) -> Exit<X86> {
    Exit::Arch(X86Exit::Io {
        port,
        size: 1,
        write: None,
    })
}

fn io_out(port: u16, value: u8) -> Exit<X86> {
    Exit::Arch(X86Exit::Io {
        port,
        size: 1,
        write: Some(u32::from(value)),
    })
}

fn uart_init() -> Vec<Exit<X86>> {
    vec![
        io_out(0x3F9, 0x00),
        io_out(0x3FB, 0x80),
        io_out(0x3F8, 0x01),
        io_out(0x3F9, 0x00),
        io_out(0x3FB, 0x03),
        io_out(0x3FA, 0xC7),
        io_out(0x3FC, 0x03),
    ]
}

fn hello_script() -> Vec<Exit<X86>> {
    let mut s = uart_init();
    for &b in HELLO {
        s.push(io_in(0x3FD));
        s.push(io_out(0x3F8, b));
    }
    s.push(io_out(0xF4, 0));
    s
}

fn vmm_with(script: Vec<Exit<X86>>) -> Vmm<MockBackend> {
    let mut mock = MockBackend::with_exits(script);
    mock.set_policy(&X86Policy {
        cpuid: cpuid_model(),
        msr_filter: msr_filter_allow(),
    })
    .unwrap();
    let ram = GuestRam::new(4096).unwrap();
    Vmm::new(mock, ram)
}

#[test]
fn hello_serial_and_clean_exit() {
    let mut vmm = vmm_with(hello_script());
    let result = vmm.run().unwrap();
    assert_eq!(result.serial, HELLO);
    assert_eq!(result.reason, TerminalReason::DebugExit { code: 0 });
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
    let mut hlt = vmm_with(vec![Exit::Common(CommonExit::Idle)]);
    assert_eq!(hlt.run().unwrap().reason, TerminalReason::Idle);
    let mut sd = vmm_with(vec![Exit::Common(CommonExit::Shutdown)]);
    assert_eq!(sd.run().unwrap().reason, TerminalReason::Shutdown);
}

#[test]
fn unmodeled_exits_fail_closed() {
    let mut pic = vmm_with(vec![io_out(0x20, 0x11)]);
    assert!(matches!(pic.run(), Err(VmmError::ContractViolation(_))));

    let mut mmio = vmm_with(vec![Exit::Common(CommonExit::Mmio {
        gpa: Gpa(0xFEE0_0000),
        size: 4,
        write: None,
    })]);
    assert!(matches!(mmio.run(), Err(VmmError::ContractViolation(_))));

    let mut tsc = vmm_with(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })]);
    assert!(matches!(tsc.run(), Err(VmmError::ContractViolation(_))));

    let mut hc = vmm_with(vec![Exit::Common(
        CommonExit::Hypercall(Default::default()),
    )]);
    assert!(matches!(hc.run(), Err(VmmError::ContractViolation(_))));
}

#[test]
fn unmodeled_in_port_fails_closed() {
    let mut vmm = vmm_with(vec![io_in(0x71)]);
    assert!(matches!(vmm.run(), Err(VmmError::ContractViolation(_))));
}

#[test]
fn non_byte_io_to_modeled_ports_fails_closed() {
    let wide_out = |port: u16, size: u8, value: u32| {
        Exit::Arch(X86Exit::Io {
            port,
            size,
            write: Some(value),
        })
    };
    let wide_in = |port: u16, size: u8| {
        Exit::Arch(X86Exit::Io {
            port,
            size,
            write: None,
        })
    };

    let mut dbg = vmm_with(vec![wide_out(0xF4, 4, 0x0000_0000)]);
    assert!(matches!(dbg.run(), Err(VmmError::ContractViolation(_))));

    let mut uart_w = vmm_with(vec![wide_out(0x3F8, 2, 0x4142)]);
    assert!(matches!(uart_w.run(), Err(VmmError::ContractViolation(_))));

    let mut uart_r = vmm_with(vec![wide_in(0x3FD, 4)]);
    assert!(matches!(uart_r.run(), Err(VmmError::ContractViolation(_))));
}

#[test]
fn deny_gp_msr_injects_fault() {
    let mut vmm = vmm_with(vec![
        Exit::Arch(X86Exit::Rdmsr { index: 0xDEAD_BEEF }),
        io_out(0xF4, 0),
    ]);
    let r = vmm.run().unwrap();
    assert_eq!(r.reason, TerminalReason::DebugExit { code: 0 });
}

#[test]
fn allow_fixed_msr_returns_constant() {
    let mut mock = MockBackend::with_exits(vec![
        Exit::Arch(X86Exit::Rdmsr { index: 0x1B }),
        io_out(0xF4, 0),
    ]);
    mock.set_policy(&X86Policy {
        cpuid: cpuid_model(),
        msr_filter: msr_filter_allow(),
    })
    .unwrap();
    let mut vmm = Vmm::new(mock, GuestRam::new(4096).unwrap());
    assert_eq!(
        vmm.run().unwrap().reason,
        TerminalReason::DebugExit { code: 0 }
    );
}

#[test]
fn state_hash_is_pure_and_covers_every_component() {
    let baseline = {
        let mut v = vmm_with(hello_script());
        v.run().unwrap();
        v
    };
    let h0 = baseline.state_hash().unwrap();
    assert_eq!(h0, baseline.state_hash().unwrap());

    let same = {
        let mut v = vmm_with(hello_script());
        v.run().unwrap();
        v
    };
    assert_eq!(h0, same.state_hash().unwrap());

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
        diff_serial.state_hash().unwrap(),
        "serial divergence breaks the hash"
    );

    let diff_code = {
        let mut script = uart_init();
        for &b in HELLO {
            script.push(io_in(0x3FD));
            script.push(io_out(0x3F8, b));
        }
        script.push(io_out(0xF4, 1));
        let mut v = vmm_with(script);
        v.run().unwrap();
        v
    };
    assert_ne!(
        h0,
        diff_code.state_hash().unwrap(),
        "terminal code divergence breaks the hash"
    );

    let diff_mem = {
        let mut mock = MockBackend::with_exits(hello_script());
        mock.set_policy(&X86Policy {
            cpuid: cpuid_model(),
            msr_filter: msr_filter_allow(),
        })
        .unwrap();
        let mut ram = GuestRam::new(4096).unwrap();
        ram.as_mut_bytes()[1234] = 0xAB;
        let mut v = Vmm::new(mock, ram);
        v.run().unwrap();
        v
    };
    assert_ne!(
        h0,
        diff_mem.state_hash().unwrap(),
        "memory divergence breaks the hash"
    );

    let diff_reg = {
        let mut mock = MockBackend::with_exits(hello_script());
        mock.set_policy(&X86Policy {
            cpuid: cpuid_model(),
            msr_filter: msr_filter_allow(),
        })
        .unwrap();
        let mut st = VcpuState::default();
        st.regs.rip = 0xDEAD_0000;
        mock.set_state(st);
        let mut v = Vmm::new(mock, GuestRam::new(4096).unwrap());
        v.run().unwrap();
        v
    };
    assert_ne!(
        h0,
        diff_reg.state_hash().unwrap(),
        "register divergence breaks the hash"
    );
}

#[test]
fn wrmsr_dispositions_serviced() {
    let mut drop_write = vmm_with(vec![
        Exit::Arch(X86Exit::Wrmsr {
            index: 0x1B,
            value: 0xFEE0_0900,
        }),
        io_out(0xF4, 0),
    ]);
    assert_eq!(
        drop_write.run().unwrap().reason,
        TerminalReason::DebugExit { code: 0 }
    );

    let mut gp = vmm_with(vec![
        Exit::Arch(X86Exit::Wrmsr {
            index: 0xDEAD_BEEF,
            value: 1,
        }),
        io_out(0xF4, 0),
    ]);
    assert_eq!(
        gp.run().unwrap().reason,
        TerminalReason::DebugExit { code: 0 }
    );

    let mut fixed = vmm_with(vec![
        Exit::Arch(X86Exit::Wrmsr {
            index: 0x17,
            value: 0,
        }),
        io_out(0xF4, 0),
    ]);
    assert_eq!(
        fixed.run().unwrap().reason,
        TerminalReason::DebugExit { code: 0 }
    );
}

#[test]
fn emulate_vtime_msr_fails_closed_both_directions() {
    for idx in [0x10u32, 0x3b] {
        let mut rd = vmm_with(vec![Exit::Arch(X86Exit::Rdmsr { index: idx })]);
        assert!(matches!(rd.run(), Err(VmmError::ContractViolation(_))));
        let mut wr = vmm_with(vec![Exit::Arch(X86Exit::Wrmsr {
            index: idx,
            value: 0,
        })]);
        assert!(matches!(wr.run(), Err(VmmError::ContractViolation(_))));
    }
}

#[test]
fn allow_stateful_msr_surfacing_fails_closed() {
    let mut rd = vmm_with(vec![Exit::Arch(X86Exit::Rdmsr { index: 0xC000_0080 })]);
    assert!(matches!(rd.run(), Err(VmmError::ContractViolation(_))));
    let mut wr = vmm_with(vec![Exit::Arch(X86Exit::Wrmsr {
        index: 0xC000_0080,
        value: 0,
    })]);
    assert!(matches!(wr.run(), Err(VmmError::ContractViolation(_))));
}

#[test]
fn cpuid_exit_serviced_from_frozen_model() {
    let mut vmm = vmm_with(vec![
        Exit::Arch(X86Exit::Cpuid {
            leaf: 1,
            subleaf: 0,
        }),
        Exit::Arch(X86Exit::Cpuid {
            leaf: 0xDEAD,
            subleaf: 0,
        }),
        io_out(0xF4, 0),
    ]);
    assert_eq!(
        vmm.run().unwrap().reason,
        TerminalReason::DebugExit { code: 0 }
    );
}

#[test]
fn step_after_terminal_is_idempotent() {
    let mut vmm = vmm_with(vec![Exit::Common(CommonExit::Idle)]);
    assert_eq!(vmm.run().unwrap().reason, TerminalReason::Idle);
    assert_eq!(vmm.step().unwrap(), Step::Terminal(TerminalReason::Idle));
}

#[test]
fn guest_ram_validation_and_accessors() {
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
    let mut mock = MockBackend::with_exits(vec![Exit::Common(CommonExit::Idle)]);
    mock.set_policy(&X86Policy {
        cpuid: cpuid_model(),
        msr_filter: msr_filter_allow(),
    })
    .unwrap();
    let mut st = VcpuState {
        mp_state: MpState::Halted,
        xsave: vec![1u8, 2, 3, 4, 5, 6, 7, 8],
        ..Default::default()
    };
    st.msrs.insert(0xC000_0080, 0x500);
    st.msrs.insert(0x277, 0x0007_0406);
    mock.set_state(st);
    let mut vmm = Vmm::new(mock, GuestRam::new(4096).unwrap());
    assert_eq!(vmm.run().unwrap().reason, TerminalReason::Idle);
    assert_eq!(vmm.state_hash().unwrap(), vmm.state_hash().unwrap());
    assert_ne!(vmm.state_hash().unwrap(), [0u8; 32]);
}

#[test]
fn state_hash_distinguishes_segment_and_event_fields() {
    let hash_with = |mutate: &dyn Fn(&mut VcpuState)| {
        let mut mock = MockBackend::with_exits(vec![Exit::Common(CommonExit::Idle)]);
        mock.set_policy(&X86Policy {
            cpuid: cpuid_model(),
            msr_filter: msr_filter_allow(),
        })
        .unwrap();
        let mut st = VcpuState::default();
        mutate(&mut st);
        mock.set_state(st);
        let mut v = Vmm::new(mock, GuestRam::new(4096).unwrap());
        v.run().unwrap();
        v.state_hash().unwrap()
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
    let hash_with = |mutate: &dyn Fn(&mut VcpuState)| {
        let mut mock = MockBackend::with_exits(vec![Exit::Common(CommonExit::Idle)]);
        mock.set_policy(&X86Policy {
            cpuid: cpuid_model(),
            msr_filter: msr_filter_allow(),
        })
        .unwrap();
        let mut st = VcpuState::default();
        mutate(&mut st);
        mock.set_state(st);
        let mut v = Vmm::new(mock, GuestRam::new(4096).unwrap());
        v.run().unwrap();
        v.state_hash().unwrap()
    };
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
