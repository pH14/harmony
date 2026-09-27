// SPDX-License-Identifier: AGPL-3.0-or-later

mod vmm;

use std::hint::black_box;
use std::time::Instant;
use vm_state::{Arm64VmState, SnapshotRecords, VmState};
use vm_state_reference::SnapshotRecords as ReferenceRecords;

fn measure<T: AsRef<[u8]>>(
    mut op: impl FnMut() -> T,
    expected: &[u8],
    kind: &str,
    arm: &str,
    sample: usize,
    iterations: usize,
) {
    let start = Instant::now();
    for _ in 0..iterations {
        assert_eq!(black_box(op()).as_ref(), expected);
    }
    let ns = start.elapsed().as_nanos() as f64 / iterations as f64;
    println!(
        "{{\"kind\":\"{kind}\",\"arm\":\"{arm}\",\"sample\":{sample},\"iterations\":{iterations},\"ns\":{ns}}}"
    );
}

fn run_case(size: usize, populated: bool, check: bool) {
    let data: Vec<_> = (0..size).map(|i| (i.wrapping_mul(37) >> 4) as u8).collect();
    let mut x86 = VmState::default();
    let mut arm64 = Arm64VmState::default();
    x86.devices.0 = data.clone();
    arm64.devices.0 = data;
    arm64.vtimer.counter = 0x1234_5678_90ab_cdef;
    if populated {
        x86.regs.rax = 0xabcdef;
        arm64.regs.x[0] = 0xabcdef;
        x86.xsave.0 = vec![0x47; 4096];
        x86.xsave_restore_bv = Some(3);
        x86.hypercall = vec![5; 64];
        arm64.hypercall = vec![5; 64];
        x86.engine_state = vec![1, 2, 3];
        arm64.engine_state = vec![1, 2, 3];
        for i in 0..32 {
            x86.msrs.0.insert(i, u64::from(i) * 73);
            x86.timers.entries.push(vm_state::TimerEntry {
                deadline_vns: u64::from(i) * 100,
                seq: u64::from(i),
                token: u64::from(i) * 3,
                period_vns: 1000,
            });
        }
        x86.timers.next_seq = 32;
        arm64.timers = x86.timers.clone();
    }
    let x86_bytes = x86.encode().unwrap();
    let arm64_bytes = arm64.encode().unwrap();
    assert_eq!(VmState::decode(&x86_bytes).unwrap(), x86);
    assert_eq!(Arm64VmState::decode(&arm64_bytes).unwrap(), arm64);
    let old_x86 = vm_state_reference::VmState::decode(&x86_bytes).unwrap();
    let old_arm64 = vm_state_reference::Arm64VmState::decode(&arm64_bytes).unwrap();
    assert_eq!(old_x86.encode().unwrap(), x86_bytes);
    assert_eq!(old_arm64.encode().unwrap(), arm64_bytes);
    let hash_bytes = arm64.encode_for_hash().unwrap();
    assert_eq!(hash_bytes, old_arm64.encode_for_hash().unwrap());
    let mut normalized = arm64.clone();
    normalized.vtimer.counter = 0;
    assert_eq!(hash_bytes, normalized.encode().unwrap());
    assert_ne!(hash_bytes, arm64_bytes);
    assert_eq!(arm64.vtimer.counter, 0x1234_5678_90ab_cdef);
    if populated {
        for mode in 0..4 {
            macro_rules! malformed {
                ($state:expr) => {{
                    let mut state = $state.clone();
                    match mode {
                        0 => state.timers.next_seq = 0,
                        1 => state.timers.entries.swap(0, 1),
                        2 => state.timers.entries[1].token = state.timers.entries[0].token,
                        _ => state.timers.entries[1] = state.timers.entries[0],
                    }
                    state
                }};
            }
            let bad_x86 = malformed!(x86);
            let bad_old_x86 = malformed!(old_x86);
            let bad_arm64 = malformed!(arm64);
            let bad_old_arm64 = malformed!(old_arm64);
            assert_eq!(
                bad_x86.encode().unwrap_err().to_string(),
                bad_old_x86.encode().unwrap_err().to_string()
            );
            assert_eq!(
                bad_arm64.encode().unwrap_err().to_string(),
                bad_old_arm64.encode().unwrap_err().to_string()
            );
            assert_eq!(
                bad_arm64.encode_for_hash().unwrap_err().to_string(),
                bad_old_arm64.encode_for_hash().unwrap_err().to_string()
            );
        }
    }
    assert_eq!(x86_bytes.capacity(), x86_bytes.len());
    assert_eq!(arm64_bytes.capacity(), arm64_bytes.len());
    assert_eq!(hash_bytes.capacity(), hash_bytes.len());
    let before = arm64.clone();
    let iterations = if check {
        1
    } else {
        (32 * 1024 * 1024 / arm64_bytes.len()).clamp(16, 5000)
    };
    for sample in 0..if check { 1 } else { 9 } {
        for old in if sample % 2 == 0 {
            [true, false]
        } else {
            [false, true]
        } {
            let name = if old { "original" } else { "current" };
            measure(
                || {
                    if old {
                        old_x86.encode().unwrap()
                    } else {
                        x86.encode().unwrap()
                    }
                },
                &x86_bytes,
                &format!("x86_{size}_{populated}"),
                name,
                sample,
                iterations,
            );
            measure(
                || {
                    if old {
                        old_arm64.encode().unwrap()
                    } else {
                        arm64.encode().unwrap()
                    }
                },
                &arm64_bytes,
                &format!("arm64_{size}_{populated}"),
                name,
                sample,
                iterations,
            );
            measure(
                || {
                    if old {
                        old_arm64.encode_for_hash().unwrap()
                    } else {
                        arm64.encode_for_hash().unwrap()
                    }
                },
                &hash_bytes,
                &format!("arm64_hash_{size}_{populated}"),
                name,
                sample,
                iterations,
            );
        }
    }
    assert_eq!(arm64, before);
    println!(
        "{{\"size\":{size},\"populated\":{populated},\"arm64_len\":{},\"original_capacity\":{},\"current_capacity\":{}}}",
        arm64_bytes.len(),
        old_arm64.encode().unwrap().capacity(),
        arm64.encode().unwrap().capacity()
    );
}

fn main() {
    let check = std::env::args().any(|x| x == "--check");
    for size in [0, 4096, 65536, 1024 * 1024] {
        for populated in [false, true] {
            run_case(size, populated, check);
        }
    }
    vmm::run(check);
    println!("Both snapshot encoders preserve bytes, records, and ARM hash normalization");
}
