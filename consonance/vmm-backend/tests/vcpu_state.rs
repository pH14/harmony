// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 3 — `VcpuState` round-trip proptest. Arbitrary state through
//! `MockBackend::restore` → `save` must reproduce `==`, and equal states must
//! save equal (`BTreeMap` order, no float). Portable; no `/dev/kvm`.
#![cfg(feature = "mock")]

use proptest::prelude::*;
use vmm_backend::{
    Backend, DebugRegs, DescriptorTable, MockBackend, MpState, Segment, VcpuEvents, VcpuRegs,
    VcpuSregs, VcpuState,
};

/// 16 cases under Miri (slow interpreter), full count natively.
fn cases(native: u32) -> ProptestConfig {
    let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 16 } else { native });
    if cfg!(miri) {
        cfg.failure_persistence = None;
    }
    cfg
}

fn arb_segment() -> impl Strategy<Value = Segment> {
    (any::<u64>(), any::<u32>(), any::<u16>(), any::<[u8; 9]>()).prop_map(
        |(base, limit, selector, f)| Segment {
            base,
            limit,
            selector,
            type_: f[0],
            present: f[1],
            dpl: f[2],
            db: f[3],
            s: f[4],
            l: f[5],
            g: f[6],
            avl: f[7],
            unusable: f[8],
        },
    )
}

fn arb_dtable() -> impl Strategy<Value = DescriptorTable> {
    (any::<u64>(), any::<u16>()).prop_map(|(base, limit)| DescriptorTable { base, limit })
}

fn arb_regs() -> impl Strategy<Value = VcpuRegs> {
    any::<[u64; 18]>().prop_map(|a| VcpuRegs {
        rax: a[0],
        rbx: a[1],
        rcx: a[2],
        rdx: a[3],
        rsi: a[4],
        rdi: a[5],
        rsp: a[6],
        rbp: a[7],
        r8: a[8],
        r9: a[9],
        r10: a[10],
        r11: a[11],
        r12: a[12],
        r13: a[13],
        r14: a[14],
        r15: a[15],
        rip: a[16],
        rflags: a[17],
    })
}

fn arb_sregs() -> impl Strategy<Value = VcpuSregs> {
    (
        proptest::array::uniform8(arb_segment()),
        proptest::array::uniform2(arb_dtable()),
        any::<[u64; 8]>(),
        any::<[u64; 4]>(),
    )
        .prop_map(|(segs, dt, scal, pdptrs)| VcpuSregs {
            cs: segs[0],
            ds: segs[1],
            es: segs[2],
            fs: segs[3],
            gs: segs[4],
            ss: segs[5],
            tr: segs[6],
            ldt: segs[7],
            gdt: dt[0],
            idt: dt[1],
            cr0: scal[0],
            cr2: scal[1],
            cr3: scal[2],
            cr4: scal[3],
            cr8: scal[4],
            efer: scal[5],
            apic_base: scal[6],
            flags: scal[7],
            pdptrs,
        })
}

fn arb_debugregs() -> impl Strategy<Value = DebugRegs> {
    any::<[u64; 7]>().prop_map(|a| DebugRegs {
        db: [a[0], a[1], a[2], a[3]],
        dr6: a[4],
        dr7: a[5],
        flags: a[6],
    })
}

fn arb_events() -> impl Strategy<Value = VcpuEvents> {
    (any::<[u8; 17]>(), any::<[u32; 3]>(), any::<u64>()).prop_map(|(b, w, payload)| VcpuEvents {
        exception_injected: b[0],
        exception_nr: b[1],
        exception_has_error_code: b[2],
        exception_pending: b[3],
        exception_error_code: w[0],
        exception_has_payload: b[4],
        exception_payload: payload,
        interrupt_injected: b[5],
        interrupt_nr: b[6],
        interrupt_soft: b[7],
        interrupt_shadow: b[8],
        nmi_injected: b[9],
        nmi_pending: b[10],
        nmi_masked: b[11],
        sipi_vector: w[1],
        flags: w[2],
        smi_smm: b[12],
        smi_pending: b[13],
        smi_inside_nmi: b[14],
        smi_latched_init: b[15],
        triple_fault_pending: b[16],
    })
}

fn arb_mp_state() -> impl Strategy<Value = MpState> {
    prop_oneof![Just(MpState::Runnable), Just(MpState::Halted)]
}

fn arb_vcpu_state() -> impl Strategy<Value = VcpuState> {
    (
        arb_regs(),
        arb_sregs(),
        any::<u64>(),
        arb_debugregs(),
        arb_events(),
        arb_mp_state(),
        proptest::collection::btree_map(any::<u32>(), any::<u64>(), 0..16),
        proptest::collection::vec(any::<u8>(), 0..1024),
    )
        .prop_map(
            |(regs, sregs, xcr0, debugregs, events, mp_state, msrs, xsave)| VcpuState {
                regs,
                sregs,
                xcr0,
                debugregs,
                events,
                mp_state,
                msrs,
                xsave,
            },
        )
}

proptest! {
    #![proptest_config(cases(256))]

    /// `restore(&s)` then `save()` reproduces `s` exactly, and a second backend
    /// restored from the same `s` saves an identical `VcpuState` (determinism:
    /// sorted MSR map, no float).
    #[test]
    fn restore_save_round_trips(s in arb_vcpu_state()) {
        let mut a = MockBackend::new();
        a.restore(&s).expect("restore");
        let out = a.save().expect("save");
        prop_assert_eq!(&out, &s);

        let mut b = MockBackend::new();
        b.restore(&s).expect("restore b");
        prop_assert_eq!(b.save().expect("save b"), a.save().expect("save a"));
    }
}
