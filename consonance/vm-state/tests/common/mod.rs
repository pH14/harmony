// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared proptest strategies and a fixed fully-populated `VmState` for the
//! integration tests. Each `tests/*.rs` file pulls in only what it needs, so
//! some helpers are unused per binary.
#![allow(dead_code)]

use proptest::prelude::*;
use vm_state::{
    DebugRegs, DeviceBlob, MpState, MsrBlock, Segment, TimerEntry, TimerQueueState, VcpuEvents,
    VcpuRegs, VcpuSregs, VmState, VtimeState, Xcrs, XsaveImage,
};

/// Per-test proptest config. Native runs keep the spec's case counts. **Under
/// Miri** two things change so `cargo +nightly miri test -p vm-state` stays
/// usable:
///
/// * **Cases are cut to 16.** The interpreter is ~10–100× slower; 16 independent
///   seeds still drive the byte-parsing and zerocopy-read paths Miri is here to
///   scrutinize for UB. The reduction is Miri-only (`cfg!(miri)`); native runs
///   honor the ≥256 convention.
/// * **Failure persistence is disabled.** proptest's default resolves a
///   regression-file path via `current_dir()` (getcwd), which Miri rejects under
///   filesystem isolation. There is no regression-replay workflow under Miri, so
///   dropping it is free; native runs keep the default file persistence.
pub fn config(native_cases: u32) -> ProptestConfig {
    let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 16 } else { native_cases });
    if cfg!(miri) {
        cfg.failure_persistence = None;
    }
    cfg
}

pub fn arb_regs() -> impl Strategy<Value = VcpuRegs> {
    proptest::collection::vec(any::<u64>(), 18..=18).prop_map(|v| VcpuRegs {
        rax: v[0],
        rbx: v[1],
        rcx: v[2],
        rdx: v[3],
        rsi: v[4],
        rdi: v[5],
        rsp: v[6],
        rbp: v[7],
        r8: v[8],
        r9: v[9],
        r10: v[10],
        r11: v[11],
        r12: v[12],
        r13: v[13],
        r14: v[14],
        r15: v[15],
        rip: v[16],
        rflags: v[17],
    })
}

pub fn arb_segment() -> impl Strategy<Value = Segment> {
    (
        any::<u64>(),
        any::<u32>(),
        any::<u16>(),
        any::<u8>(),
        any::<u8>(),
        any::<u8>(),
    )
        .prop_map(
            |(base, limit, selector, type_, present_dpl_s, flags)| Segment {
                base,
                limit,
                selector,
                type_,
                present_dpl_s,
                flags,
            },
        )
}

pub fn arb_sregs() -> impl Strategy<Value = VcpuSregs> {
    (
        proptest::collection::vec(arb_segment(), 8..=8),
        (
            any::<u64>(),
            any::<u16>(),
            any::<u64>(),
            any::<u16>(),
            any::<u64>(),
            any::<u64>(),
            any::<u64>(),
            any::<u64>(),
            any::<u64>(),
            any::<u64>(),
        ),
        any::<u64>(),
    )
        .prop_map(|(seg, scal, apic_base)| VcpuSregs {
            cs: seg[0],
            ds: seg[1],
            es: seg[2],
            fs: seg[3],
            gs: seg[4],
            ss: seg[5],
            tr: seg[6],
            ldt: seg[7],
            gdt_base: scal.0,
            gdt_limit: scal.1,
            idt_base: scal.2,
            idt_limit: scal.3,
            cr0: scal.4,
            cr2: scal.5,
            cr3: scal.6,
            cr4: scal.7,
            cr8: scal.8,
            efer: scal.9,
            apic_base,
        })
}

pub fn arb_xcrs() -> impl Strategy<Value = Xcrs> {
    any::<u64>().prop_map(|xcr0| Xcrs { xcr0 })
}

pub fn arb_debugregs() -> impl Strategy<Value = DebugRegs> {
    (
        proptest::collection::vec(any::<u64>(), 4..=4),
        any::<u64>(),
        any::<u64>(),
    )
        .prop_map(|(db, dr6, dr7)| DebugRegs {
            db: [db[0], db[1], db[2], db[3]],
            dr6,
            dr7,
        })
}

pub fn arb_events() -> impl Strategy<Value = VcpuEvents> {
    (
        any::<bool>(),
        any::<u8>(),
        any::<u32>(),
        any::<bool>(),
        any::<bool>(),
        any::<u8>(),
    )
        .prop_map(
            |(
                exception_pending,
                exception_vector,
                exception_error_code,
                nmi_pending,
                smi_pending,
                interrupt_shadow,
            )| VcpuEvents {
                exception_pending,
                exception_vector,
                exception_error_code,
                nmi_pending,
                smi_pending,
                interrupt_shadow,
            },
        )
}

pub fn arb_mp_state() -> impl Strategy<Value = MpState> {
    prop_oneof![Just(MpState::Runnable), Just(MpState::Halted)]
}

pub fn arb_msrs() -> impl Strategy<Value = MsrBlock> {
    proptest::collection::btree_map(any::<u32>(), any::<u64>(), 0..16).prop_map(MsrBlock)
}

pub fn arb_xsave() -> impl Strategy<Value = XsaveImage> {
    proptest::collection::vec(any::<u8>(), 0..64).prop_map(XsaveImage)
}

pub fn arb_vtime() -> impl Strategy<Value = VtimeState> {
    (any::<u64>(), any::<u64>(), any::<u64>(), any::<u64>()).prop_map(
        |(ratio_num, tsc_hz, tsc_base, snapshot_vns)| VtimeState {
            ratio_num,
            // Only integer-ratio configs are encodable (INTEGRATION.md §4).
            ratio_den: 1,
            tsc_hz,
            tsc_base,
            snapshot_vns,
        },
    )
}

pub fn arb_timers() -> impl Strategy<Value = TimerQueueState> {
    // A faithful restored TimerQueue: distinct seqs (assigned 0..n), distinct
    // tokens, arbitrary deadlines/periods, `next_seq` strictly above every seq,
    // entries in canonical (deadline_vns, seq) order — all the invariants the
    // codec enforces (see `validate_timers`).
    (0usize..16)
        .prop_flat_map(|n| {
            (
                proptest::collection::vec(any::<u64>(), n), // deadlines
                proptest::collection::vec(any::<u64>(), n), // periods
                proptest::collection::btree_set(any::<u64>(), n..=n), // distinct tokens
                any::<u64>(),                               // next_seq slack
            )
        })
        .prop_map(|(deadlines, periods, tokens, slack)| {
            let n = deadlines.len();
            let tokens: Vec<u64> = tokens.into_iter().collect();
            let mut entries: Vec<TimerEntry> = (0..n)
                .map(|i| TimerEntry {
                    deadline_vns: deadlines[i],
                    seq: i as u64,
                    token: tokens[i],
                    period_vns: periods[i],
                })
                .collect();
            entries.sort_by_key(|e| (e.deadline_vns, e.seq));
            // next_seq strictly above every seq (max seq is n-1), with slack.
            let next_seq = (n as u64).saturating_add(slack % 4096);
            TimerQueueState { entries, next_seq }
        })
}

pub fn arb_hypercall() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..64)
}

pub fn arb_devices() -> impl Strategy<Value = DeviceBlob> {
    proptest::collection::vec(any::<u8>(), 0..64).prop_map(DeviceBlob)
}

pub fn arb_contract_hash() -> impl Strategy<Value = [u8; 32]> {
    proptest::collection::vec(any::<u8>(), 32..=32).prop_map(|v| {
        let mut a = [0u8; 32];
        a.copy_from_slice(&v);
        a
    })
}

pub fn arb_vm_state() -> impl Strategy<Value = VmState> {
    (
        arb_regs(),
        arb_sregs(),
        arb_xcrs(),
        arb_debugregs(),
        arb_events(),
        arb_mp_state(),
        arb_msrs(),
        arb_xsave(),
        arb_vtime(),
        arb_timers(),
    )
        .prop_flat_map(
            |(regs, sregs, xcrs, debugregs, events, mp_state, msrs, xsave, vtime, timers)| {
                (arb_hypercall(), arb_devices(), arb_contract_hash()).prop_map(
                    move |(hypercall, devices, contract_hash)| VmState {
                        regs,
                        sregs,
                        xcrs,
                        debugregs,
                        events,
                        mp_state,
                        msrs: msrs.clone(),
                        xsave: xsave.clone(),
                        vtime,
                        timers: timers.clone(),
                        hypercall,
                        devices,
                        contract_hash,
                    },
                )
            },
        )
}

/// A fixed, fully-populated `VmState` with a non-trivial value in every field —
/// the input for the golden-stability, version-rejection, and ratio-rejection
/// tests. Deterministic and small.
pub fn fully_populated() -> VmState {
    let seg = |n: u64| Segment {
        base: 0x1000 * n,
        limit: 0x10 + n as u32,
        selector: 0x8 * n as u16,
        type_: 0x0b,
        present_dpl_s: 0x9b,
        flags: 0x20,
    };
    let mut msrs = std::collections::BTreeMap::new();
    msrs.insert(0x0000_0010u32, 0x1122_3344_5566_7788u64); // IA32_TSC
    msrs.insert(0x0000_0174u32, 0x0000_0000_0000_0008u64); // IA32_SYSENTER_CS
    msrs.insert(0xC000_0080u32, 0x0000_0000_0000_0501u64); // IA32_EFER

    VmState {
        regs: VcpuRegs {
            rax: 0x0000_0000_0000_0001,
            rbx: 0x0000_0000_0000_0002,
            rcx: 0x0000_0000_0000_0003,
            rdx: 0x0000_0000_0000_0004,
            rsi: 0x0000_0000_0000_0005,
            rdi: 0x0000_0000_0000_0006,
            rsp: 0x0000_7fff_ffff_e000,
            rbp: 0x0000_7fff_ffff_e100,
            r8: 0x0000_0000_0000_0008,
            r9: 0x0000_0000_0000_0009,
            r10: 0x0000_0000_0000_000a,
            r11: 0x0000_0000_0000_000b,
            r12: 0x0000_0000_0000_000c,
            r13: 0x0000_0000_0000_000d,
            r14: 0x0000_0000_0000_000e,
            r15: 0x0000_0000_0000_000f,
            rip: 0xffff_ffff_8100_0000,
            rflags: 0x0000_0000_0000_0202,
        },
        sregs: VcpuSregs {
            cs: seg(1),
            ds: seg(2),
            es: seg(3),
            fs: seg(4),
            gs: seg(5),
            ss: seg(6),
            tr: seg(7),
            ldt: seg(8),
            gdt_base: 0xffff_ffff_8200_0000,
            gdt_limit: 0x7f,
            idt_base: 0xffff_ffff_8300_0000,
            idt_limit: 0xfff,
            cr0: 0x8005_0033,
            cr2: 0x0000_0000_dead_beef,
            cr3: 0x0000_0000_0100_0000,
            cr4: 0x0000_0000_0020_06b0,
            cr8: 0x0000_0000_0000_0000,
            efer: 0x0000_0000_0000_0d01,
            apic_base: 0x0000_0000_fee0_0900,
        },
        xcrs: Xcrs {
            xcr0: 0x0000_0000_0000_0007,
        },
        debugregs: DebugRegs {
            db: [0x10, 0x20, 0x30, 0x40],
            dr6: 0x0000_0000_ffff_0ff0,
            dr7: 0x0000_0000_0000_0400,
        },
        events: VcpuEvents {
            exception_pending: true,
            exception_vector: 14,
            exception_error_code: 0x0000_0002,
            nmi_pending: false,
            smi_pending: true,
            interrupt_shadow: 0b10,
        },
        mp_state: MpState::Halted,
        msrs: MsrBlock(msrs),
        xsave: XsaveImage(vec![0x7f, 0x1f, 0x00, 0x00, 0xaa, 0xbb, 0xcc, 0xdd]),
        vtime: VtimeState {
            ratio_num: 2,
            ratio_den: 1,
            tsc_hz: 2_000_000_000,
            tsc_base: 0,
            snapshot_vns: 0x0000_0000_075b_cd15, // 123_456_789
        },
        timers: TimerQueueState {
            entries: vec![
                TimerEntry {
                    deadline_vns: 1000,
                    seq: 0,
                    token: 7,
                    period_vns: 0,
                },
                TimerEntry {
                    deadline_vns: 1000,
                    seq: 1,
                    token: 9,
                    period_vns: 500,
                },
                TimerEntry {
                    deadline_vns: 2000,
                    seq: 2,
                    token: 3,
                    period_vns: 0,
                },
            ],
            next_seq: 3,
        },
        hypercall: vec![0x01, 0x02, 0x03, 0x04, 0x05],
        devices: DeviceBlob(vec![0x55, 0x66, 0x77]),
        contract_hash: [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
            0x1c, 0x1d, 0x1e, 0x1f,
        ],
    }
}
