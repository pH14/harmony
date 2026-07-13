// SPDX-License-Identifier: AGPL-3.0-or-later
//! `det-corpus` oracles over the VMM-backed [`vmm_core::corpus::CorpusMachine`],
//! driven by a scripted `MockBackend` — every platform, no `/dev/kvm` (corpus
//! box-integration, task 28).
//!
//! This is the macOS-runnable companion to the box-only `box_corpus` gate: it
//! wires the **same** `det-corpus` oracle runner (`check_determinism`,
//! `check_conformance`) to the **same** `CorpusMachine` bridge, just over a
//! scripted backend instead of the live patched KVM. So the cross-crate
//! integration — that the bridge satisfies the `unison::Subject`/`SubjectFactory`
//! contracts the `det-corpus` generics demand, and that O1 (state_hash) and the
//! O2 observable digest behave as the corpus expects — is type-checked and gated
//! on every platform; only the live-KVM values are box-only.

use det_corpus::{check_conformance, check_determinism};
use unison::SubjectFactory;
use vmm_backend::{Backend, CpuidModel, Exit, MockBackend, MsrFilter};
use vmm_core::corpus::{CorpusMachine, observable_digest_of};
use vmm_core::devices::{ISA_DEBUG_EXIT_PORT, REPORT_PORT, UART_PORT_BASE};
use vmm_core::vmm::{GuestRam, Vmm};

/// Build a deterministic scripted run: emit `name`'s serial banner, report each of
/// `values` (two report-port dwords, low then high), then a clean PASS.
fn script(name: &str, values: &[u64]) -> Vec<Exit> {
    let mut exits = Vec::new();
    for &b in format!("PAYLOAD {name} PASS\n").as_bytes() {
        exits.push(Exit::Io {
            port: UART_PORT_BASE,
            size: 1,
            write: Some(u32::from(b)),
        });
    }
    for &v in values {
        exits.push(Exit::Io {
            port: REPORT_PORT,
            size: 4,
            write: Some(v as u32),
        });
        exits.push(Exit::Io {
            port: REPORT_PORT,
            size: 4,
            write: Some((v >> 32) as u32),
        });
    }
    exits.push(Exit::Io {
        port: ISA_DEBUG_EXIT_PORT,
        size: 1,
        write: Some(0),
    });
    exits
}

/// A `unison::SubjectFactory` over a scripted `MockBackend` `CorpusMachine` — the
/// no-`/dev/kvm` stand-in for `box_corpus`'s patched-backend factory.
struct MockCorpusFactory {
    name: String,
    values: Vec<u64>,
}

impl SubjectFactory for MockCorpusFactory {
    type M = CorpusMachine<MockBackend>;
    fn spawn(&self, _seed: u64) -> Self::M {
        let mut backend = MockBackend::with_exits(script(&self.name, &self.values));
        backend
            .set_cpuid(&CpuidModel::default())
            .expect("set_cpuid");
        backend
            .set_msr_filter(&MsrFilter::default())
            .expect("set_msr_filter");
        CorpusMachine::new(Vmm::new(backend, GuestRam::new(0x1000).unwrap()))
    }
}

/// The **boxed** factory — `type M = CorpusMachine<Box<dyn Backend>>` — mirrors
/// `box_corpus`'s `PatchedPayloadFactory` exactly (it spawns
/// `CorpusMachine<Box<dyn Backend>>` from `boot_patched_payload`), so this
/// type-checks that generic plumbing + `det-corpus` over it on every platform.
struct BoxedMockFactory {
    name: String,
    values: Vec<u64>,
}

impl SubjectFactory for BoxedMockFactory {
    type M = CorpusMachine<Box<dyn Backend>>;
    fn spawn(&self, _seed: u64) -> Self::M {
        let mut backend = MockBackend::with_exits(script(&self.name, &self.values));
        backend
            .set_cpuid(&CpuidModel::default())
            .expect("set_cpuid");
        backend
            .set_msr_filter(&MsrFilter::default())
            .expect("set_msr_filter");
        let boxed: Box<dyn Backend> = Box::new(backend);
        CorpusMachine::new(Vmm::new(boxed, GuestRam::new(0x1000).unwrap()))
    }
}

#[test]
fn det_corpus_o1_over_the_boxed_bridge_compiles_and_passes() {
    // Exercises `CorpusMachine<Box<dyn Backend>>` (the box runner's exact type)
    // through the real det-corpus runner — the generic plumbing box_corpus relies
    // on, verified with no /dev/kvm.
    let f = BoxedMockFactory {
        name: "insn-cpuid".to_string(),
        values: vec![0x0009_06ec, 7],
    };
    let o1 = check_determinism(&f, 1, 4096, 1_000_000).expect("check_determinism");
    assert!(o1.passed, "boxed-bridge O1 must pass: {}", o1.detail);
}

#[test]
fn det_corpus_o1_passes_over_the_vmm_bridge() {
    // O1 (determinism) via the real det-corpus runner: two runs at one seed must
    // be bit-identical. This is exactly the call `box_corpus` makes on the box.
    let f = MockCorpusFactory {
        name: "insn-rdtsc".to_string(),
        values: vec![64, 0xAABB_CCDD_0011_2233, 0xDEAD_BEEF],
    };
    let o1 =
        check_determinism(&f, 0x0028_C0FF_EE5E_EDC0, 4096, 1_000_000).expect("check_determinism");
    assert!(o1.passed, "O1 over the VMM bridge must pass: {}", o1.detail);
}

#[test]
fn det_corpus_o2_digest_matches_the_observable_golden() {
    // O2 (conformance), corpus box-integration semantics: the run's
    // observable_digest (report stream + serial) is the golden. Recompute the
    // expected digest independently from the known stream + banner and confirm
    // the bridge's observable_digest agrees — the box runner pins exactly this.
    use unison::Subject;
    let name = "insn-rng";
    let values = vec![0x1111_2222_3333_4444u64, 5];
    let mut m = MockCorpusFactory {
        name: name.to_string(),
        values: values.clone(),
    }
    .spawn(0);
    m.run_to(u64::MAX).expect("run_to");

    // The expected report stream: each value as (low, high) dwords, in order.
    let mut stream = Vec::new();
    for &v in &values {
        stream.push(v as u32);
        stream.push((v >> 32) as u32);
    }
    let expected = observable_digest_of(&stream, format!("PAYLOAD {name} PASS\n").as_bytes());
    assert_eq!(
        m.observable_digest(),
        expected,
        "the bridge's observable_digest must equal the recomputed report+serial digest"
    );

    // And det-corpus's check_conformance is a Fail (not a panic) on a digest that
    // is NOT the golden — the negative direction the box gate relies on. (O2 here
    // uses the corpus runner's hex-golden compare against state_hash; the
    // box-integration O2 instead pins observable_digest, but this confirms the
    // runner is robust on a non-matching golden.)
    let f = MockCorpusFactory {
        name: name.to_string(),
        values,
    };
    let bogus = "0".repeat(64);
    let o2 = check_conformance(&f, 0, 1_000_000, &bogus).expect("check_conformance");
    assert!(!o2.passed, "a non-matching golden must Fail, not pass");
}
