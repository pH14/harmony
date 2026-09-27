// SPDX-License-Identifier: AGPL-3.0-or-later

use super::{ALLOCATIONS, measure};
use std::hint::black_box;
use std::sync::atomic::Ordering;
use vmm_backend::{Arm64AsidBits, Arm64Caps, Backend, Capabilities, MockArm64Backend};
use vmm_core::vendor::arm64::contract::contract_hash as cached;
use vmm_core_reference::vendor::arm64::contract::contract_hash as uncached;

fn backend(asid_bits: Arm64AsidBits) -> MockArm64Backend {
    let mut backend = MockArm64Backend::with_capabilities(Capabilities {
        name: "mock-arm64",
        arch: Arm64Caps {
            in_kernel_gic: false,
            asid_bits,
        },
    });
    backend
        .set_policy(&vmm_core::vendor::arm64::contract::policy(asid_bits))
        .unwrap();
    backend
}

fn vmm(len: usize, check: bool, asid_bits: Arm64AsidBits) {
    let mut ram = vmm_core::vmm::GuestRam::new(len).unwrap();
    let mut reference_ram = vmm_core_reference::vmm::GuestRam::new(len).unwrap();
    for (i, byte) in ram.as_mut_bytes().iter_mut().enumerate() {
        *byte = (i.wrapping_mul(37) >> 5) as u8;
    }
    reference_ram.as_mut_bytes().copy_from_slice(ram.as_bytes());
    let memory = ram.as_bytes().to_vec();
    let mut a = vmm_core::vmm::Vmm::new(backend(asid_bits), ram);
    let mut b = vmm_core_reference::vmm::Vmm::new(backend(asid_bits), reference_ram);
    a.wire_snapshot_hashing();
    b.wire_snapshot_hashing();
    let expected_blob = b.state_blob().unwrap();
    let expected_hash = b.state_hash().unwrap();
    let state = b.save_vm_state().unwrap();
    let expected_state = state.clone();
    assert_eq!(a.state_blob().unwrap(), expected_blob);
    assert_eq!(a.state_hash().unwrap(), expected_hash);
    assert_eq!(a.save_vm_state().unwrap(), expected_state);
    let mut different = state.clone();
    different.regs.x[0] ^= 0x1234;
    a.restore_vm_state(&different).unwrap();
    b.restore_vm_state(&different).unwrap();
    assert_ne!(a.state_hash().unwrap(), expected_hash);
    assert_ne!(b.state_hash().unwrap(), expected_hash);
    a.restore_snapshot(&memory, &state).unwrap();
    b.restore_snapshot(&memory, &state).unwrap();
    assert_eq!(a.state_blob().unwrap(), expected_blob);
    assert_eq!(b.state_blob().unwrap(), expected_blob);
    let other_width = match asid_bits {
        Arm64AsidBits::Eight => Arm64AsidBits::Sixteen,
        Arm64AsidBits::Sixteen => Arm64AsidBits::Eight,
    };
    let mut other_contract = state.clone();
    other_contract.contract_hash = cached(other_width);
    assert!(a.restore_vm_state(&other_contract).is_err());
    assert!(b.restore_vm_state(&other_contract).is_err());
    assert_eq!(a.state_blob().unwrap(), expected_blob);
    assert_eq!(b.state_blob().unwrap(), expected_blob);
    let mut wrong_contract = state.clone();
    wrong_contract.contract_hash[0] ^= 1;
    assert!(a.restore_vm_state(&wrong_contract).is_err());
    assert!(b.restore_vm_state(&wrong_contract).is_err());
    assert_eq!(a.state_blob().unwrap(), expected_blob);
    assert_eq!(b.state_blob().unwrap(), expected_blob);
    for sample in 0..if check { 1 } else { 9 } {
        for arm in if sample % 2 == 0 { [0, 1] } else { [1, 0] } {
            let name = if arm == 0 { "uncached" } else { "cached" };
            let iterations = if check {
                1
            } else {
                (16 * 1024 * 1024 / len).clamp(1, 1000)
            };
            measure(
                || {
                    let value = if arm == 0 {
                        b.state_hash().unwrap()
                    } else {
                        a.state_hash().unwrap()
                    };
                    assert_eq!(black_box(value), expected_hash);
                },
                &format!("arm64_{asid_bits:?}_hash_{len}"),
                name,
                sample,
                iterations,
            );
            measure(
                || {
                    let value = if arm == 0 {
                        b.save_vm_state().unwrap()
                    } else {
                        a.save_vm_state().unwrap()
                    };
                    assert_eq!(black_box(value), expected_state);
                },
                &format!("arm64_{asid_bits:?}_capture_{len}"),
                name,
                sample,
                iterations,
            );
            measure(
                || {
                    if arm == 0 {
                        b.restore_snapshot(black_box(&memory), black_box(&state))
                            .unwrap();
                    } else {
                        a.restore_snapshot(black_box(&memory), black_box(&state))
                            .unwrap();
                    }
                },
                &format!("arm64_{asid_bits:?}_restore_{len}"),
                name,
                sample,
                iterations,
            );
            assert_eq!(a.state_blob().unwrap(), expected_blob);
            assert_eq!(b.state_blob().unwrap(), expected_blob);
        }
    }
}

pub fn run(args: &[String]) {
    let widths = [Arm64AsidBits::Eight, Arm64AsidBits::Sixteen];
    if args.iter().any(|x| x == "--concurrent") {
        let barrier = std::sync::Barrier::new(8);
        let results = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|i| {
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        let width = widths[i % widths.len()];
                        (width, cached(width))
                    })
                })
                .collect();
            threads
                .into_iter()
                .map(|t| t.join().unwrap())
                .collect::<Vec<_>>()
        });
        for (width, hash) in results {
            assert_eq!(hash, uncached(width));
        }
        assert_ne!(cached(widths[0]), cached(widths[1]));
        println!("Concurrent ARM first callers preserve both ASID identities");
        return;
    }
    if args.iter().any(|x| x == "--first-use") {
        let arm = args.last().unwrap().as_str();
        for width in widths {
            let mut value = [0; 32];
            measure(
                || {
                    value = if arm == "cached" {
                        cached(width)
                    } else {
                        uncached(width)
                    }
                },
                &format!("arm64_{width:?}_first_use"),
                arm,
                0,
                1,
            );
            assert_eq!(
                value,
                if arm == "cached" {
                    uncached(width)
                } else {
                    cached(width)
                }
            );
        }
        return;
    }
    let check = args.iter().any(|x| x == "--check");
    for width in widths {
        let expected = uncached(width);
        assert_eq!(cached(width), expected);
        let before = ALLOCATIONS.load(Ordering::Relaxed);
        for _ in 0..1000 {
            assert_eq!(black_box(cached(black_box(width))), expected);
        }
        assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), before);
        if !check {
            for sample in 0..9 {
                for arm in if sample % 2 == 0 { [0, 1] } else { [1, 0] } {
                    measure(
                        || {
                            assert_eq!(
                                black_box(if arm == 0 {
                                    uncached(black_box(width))
                                } else {
                                    cached(black_box(width))
                                }),
                                expected
                            )
                        },
                        &format!("arm64_{width:?}_fingerprint"),
                        if arm == 0 { "uncached" } else { "cached" },
                        sample,
                        10000,
                    );
                }
            }
        }
        for len in if check {
            vec![4096]
        } else {
            vec![4096, 65536, 1024 * 1024, 128 * 1024 * 1024]
        } {
            vmm(len, check, width);
        }
    }
    println!("ARM contract and snapshot comparisons passed for both ASID widths");
}
