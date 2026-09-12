// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 4 — object-safety / dyn-compatibility **and** the `impl Backend for
//! Box<B>` blanket-forward. The composition root holds a
//! `Box<dyn Backend<A = X86>>` and injects the concrete backend at `fn main`; this test
//! constructs one and drives **every** trait method through it, so each blanket
//! forward is exercised with a trait-observable assertion (a mutant that drops a
//! forward is caught: a skipped completion leaves the exit pending, so the next
//! `run` fails `PendingCompletion`; a skipped config makes `run` fail
//! `NotConfigured`; etc.). Compilation is itself the object-safety assertion.
#![cfg(feature = "mock")]

use vmm_backend::{
    Backend, CommonExit, CpuidModel, Exit, Gpa, HypercallFrame, Injection, MockBackend, MsrFilter,
    VcpuState, X86, X86Completion, X86Exit, X86Policy,
};

/// Compiles only while `Backend` is dyn-compatible (no generic methods, no
/// `Self`-by-value returns). `Box<dyn Backend<A = X86>>: Backend` is proven by the test
/// body, which drives the blanket impl directly.
fn _assert_object_safe(_: &dyn Backend<A = X86>) {}

#[test]
fn boxed_backend_forwards_every_method() {
    let script = [
        Exit::Arch(X86Exit::Cpuid {
            leaf: 1,
            subleaf: 0,
        }),
        Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
        Exit::Arch(X86Exit::Wrmsr {
            index: 0x20,
            value: 7,
        }),
        Exit::Arch(X86Exit::Wrmsr {
            index: 0x30,
            value: 9,
        }),
        Exit::Common(CommonExit::Hypercall(HypercallFrame::default())),
        Exit::Arch(X86Exit::Io {
            port: 0x3F8,
            size: 1,
            write: None,
        }),
    ];
    let mut backend: Box<dyn Backend<A = X86>> = Box::new(MockBackend::with_exits(script));

    backend
        .set_policy(&X86Policy {
            cpuid: CpuidModel::default(),
            msr_filter: MsrFilter::default(),
        })
        .unwrap();

    let mut mem = vec![0u8; 4096];
    // SAFETY: `mem` outlives the backend and is not aliased; the mock only records.
    unsafe { backend.map_memory(Gpa(0), &mut mem) }.unwrap();
    let mut bad = vec![0u8; 4096];
    // SAFETY: as above; this call is expected to error (misaligned gpa).
    assert!(unsafe { backend.map_memory(Gpa(1), &mut bad) }.is_err());

    assert_eq!(
        backend.run().unwrap(),
        Exit::Arch(X86Exit::Cpuid {
            leaf: 1,
            subleaf: 0
        })
    );
    backend
        .complete_arch(X86Completion::Cpuid {
            eax: 0xA,
            ebx: 0xB,
            ecx: 0xC,
            edx: 0xD,
        })
        .unwrap();
    backend.retire_pending_completion().unwrap();

    assert_eq!(
        backend.run().unwrap(),
        Exit::Arch(X86Exit::Rdmsr { index: 0x10 })
    );
    backend.complete_read(0x42).unwrap();

    assert_eq!(
        backend.run().unwrap(),
        Exit::Arch(X86Exit::Wrmsr {
            index: 0x20,
            value: 7
        })
    );
    backend.complete_ok().unwrap();

    assert_eq!(
        backend.run().unwrap(),
        Exit::Arch(X86Exit::Wrmsr {
            index: 0x30,
            value: 9
        })
    );
    backend.complete_fault().unwrap();

    assert_eq!(
        backend.run().unwrap(),
        Exit::Common(CommonExit::Hypercall(HypercallFrame::default()))
    );
    backend.complete_hypercall(0x99).unwrap();

    assert_eq!(
        backend.run().unwrap(),
        Exit::Arch(X86Exit::Io {
            port: 0x3F8,
            size: 1,
            write: None
        })
    );
    backend.complete_read(0x55).unwrap();
    backend.retire_pending_completion().unwrap();

    backend.inject(Injection::Nmi).unwrap();

    assert_eq!(backend.exit_counts().total(), 6);
    backend.reset_exit_counts();
    assert_eq!(backend.exit_counts().total(), 0);

    assert_eq!(backend.capabilities().name, "mock");

    let mut state = VcpuState::default();
    state.regs.rax = 0xDEAD_BEEF;
    backend.restore(&state).unwrap();
    assert_eq!(backend.save().unwrap().regs.rax, 0xDEAD_BEEF);
}

#[test]
fn injection_forwards_through_box() {
    let mut backend: Box<dyn Backend<A = X86>> =
        Box::new(MockBackend::with_exits(vec![Exit::Common(
            CommonExit::Idle,
        )]));
    backend
        .set_policy(&X86Policy {
            cpuid: CpuidModel::default(),
            msr_filter: MsrFilter::default(),
        })
        .unwrap();

    backend.set_pending_irq(Some(0x40)).unwrap();
    assert_eq!(backend.run().unwrap(), Exit::Common(CommonExit::Idle));
    assert_eq!(backend.take_accepted_interrupt(), Some(0x40));
    assert_eq!(backend.take_accepted_interrupt(), None);
}
