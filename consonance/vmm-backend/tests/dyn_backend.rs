// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 4 — object-safety / dyn-compatibility **and** the `impl Backend for
//! Box<B>` blanket-forward (task 21). The composition root holds a
//! `Box<dyn Backend>` and injects the concrete backend at `fn main`; this test
//! constructs one and drives **every** trait method through it, so each blanket
//! forward is exercised with a trait-observable assertion (a mutant that drops a
//! forward is caught: a skipped completion leaves the exit pending, so the next
//! `run` fails `PendingCompletion`; a skipped config makes `run` fail
//! `NotConfigured`; etc.). Compilation is itself the object-safety assertion.
#![cfg(feature = "mock")]

use vmm_backend::{
    Backend, CpuidModel, Exit, Gpa, HypercallRegs, Injection, MockBackend, Moment, MsrFilter,
    VcpuState,
};

/// Compiles only while `Backend` is dyn-compatible (no generic methods, no
/// `Self`-by-value returns). `Box<dyn Backend>: Backend` is proven by the test
/// body, which drives the blanket impl directly.
fn _assert_object_safe(_: &dyn Backend) {}

#[test]
fn boxed_backend_forwards_every_method() {
    // A scripted run that needs one of each completion, so every `Box<B>` forward
    // is driven and observed. Each exit is followed by another op, so a dropped
    // completion forward surfaces as a `PendingCompletion` on the next `run`.
    let script = [
        Exit::Cpuid {
            leaf: 1,
            subleaf: 0,
        },
        Exit::Rdmsr { index: 0x10 },
        Exit::Wrmsr {
            index: 0x20,
            value: 7,
        },
        Exit::Wrmsr {
            index: 0x30,
            value: 9,
        },
        Exit::Hypercall(HypercallRegs::default()),
        Exit::Io {
            port: 0x3F8,
            size: 1,
            write: None,
        },
        Exit::Deadline { reached: Moment(0) },
    ];
    let mut backend: Box<dyn Backend> = Box::new(MockBackend::with_exits(script));

    // set_cpuid / set_msr_filter forwards: if either is dropped, `run` below fails
    // `NotConfigured`.
    backend.set_cpuid(&CpuidModel::default()).unwrap();
    backend.set_msr_filter(&MsrFilter::default()).unwrap();

    // map_memory forward (an `unsafe fn` even through `dyn`): a valid map succeeds,
    // and a misaligned one errors — a dropped forward would skip the mock's
    // validation and wrongly return `Ok`.
    let mut mem = vec![0u8; 4096];
    // SAFETY: `mem` outlives the backend and is not aliased; the mock only records.
    unsafe { backend.map_memory(Gpa(0), &mut mem) }.unwrap();
    let mut bad = vec![0u8; 4096];
    // SAFETY: as above; this call is expected to error (misaligned gpa).
    assert!(unsafe { backend.map_memory(Gpa(1), &mut bad) }.is_err());

    // Each exit → its matching completion → the next `run` must succeed (proving
    // the completion forward landed).
    assert_eq!(
        backend.run().unwrap(),
        Exit::Cpuid {
            leaf: 1,
            subleaf: 0
        }
    );
    backend.complete_cpuid(0xA, 0xB, 0xC, 0xD).unwrap();

    assert_eq!(backend.run().unwrap(), Exit::Rdmsr { index: 0x10 });
    backend.complete_read(0x42).unwrap();

    assert_eq!(
        backend.run().unwrap(),
        Exit::Wrmsr {
            index: 0x20,
            value: 7
        }
    );
    backend.complete_ok().unwrap();

    assert_eq!(
        backend.run().unwrap(),
        Exit::Wrmsr {
            index: 0x30,
            value: 9
        }
    );
    backend.complete_fault().unwrap();

    assert_eq!(
        backend.run().unwrap(),
        Exit::Hypercall(HypercallRegs::default())
    );
    backend.complete_hypercall(0x99).unwrap();

    assert_eq!(
        backend.run().unwrap(),
        Exit::Io {
            port: 0x3F8,
            size: 1,
            write: None
        }
    );
    backend.complete_read(0x55).unwrap();

    // run_until forward: the mock returns `Deadline` with the requested deadline,
    // so a dropped forward (or wrong value) fails this assertion.
    assert_eq!(
        backend.run_until(Moment(5)).unwrap(),
        Exit::Deadline { reached: Moment(5) }
    );

    // inject forward: exercised through the box (its effect is not trait-observable
    // — see `.cargo/mutants.toml` exclude for the forward).
    backend.inject(Injection::Nmi).unwrap();

    // exit_counts forward: 7 exits delivered. reset_exit_counts forward: back to 0.
    assert_eq!(backend.exit_counts().total(), 7);
    backend.reset_exit_counts();
    assert_eq!(backend.exit_counts().total(), 0);

    // capabilities forward.
    assert_eq!(backend.capabilities().name, "mock");

    // save / restore forwards: restore a distinctive state through the box, then
    // save it back and confirm it round-trips (a dropped restore leaves the prior
    // state; a dropped/Default save returns the wrong value).
    let mut state = VcpuState::default();
    state.regs.rax = 0xDEAD_BEEF;
    backend.restore(&state).unwrap();
    assert_eq!(backend.save().unwrap().regs.rax, 0xDEAD_BEEF);
}

#[test]
fn injection_forwards_through_box() {
    // `set_pending_irq` + `take_accepted_interrupt` forwards through `Box<dyn
    // Backend>`: a dropped `set_pending_irq` leaves nothing pending (so the entry
    // accepts nothing and `take_accepted_interrupt` → `None`); a dropped/Defaulted
    // `take_accepted_interrupt` returns the wrong value. Both are trait-observable
    // here, so the box forwards are killable (unlike the effect-only `inject`).
    let mut backend: Box<dyn Backend> = Box::new(MockBackend::with_exits(vec![Exit::Hlt]));
    backend.set_cpuid(&CpuidModel::default()).unwrap();
    backend.set_msr_filter(&MsrFilter::default()).unwrap();

    backend.set_pending_irq(Some(0x40)).unwrap();
    assert_eq!(backend.run().unwrap(), Exit::Hlt); // mock accepts the pending IRQ
    assert_eq!(backend.take_accepted_interrupt(), Some(0x40));
    assert_eq!(backend.take_accepted_interrupt(), None);
}
