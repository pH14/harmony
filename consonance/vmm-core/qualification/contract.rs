// SPDX-License-Identifier: AGPL-3.0-or-later

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use vmm_backend::arch::x86::{CpuidModel, MsrFilter, X86Policy};
use vmm_backend::{Backend, MockBackend};
use vmm_core::vendor::x86::contract::contract_hash as cached;
use vmm_core_reference::vendor::x86::contract::contract_hash as uncached;

struct CountingAllocator;
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every allocation and deallocation is forwarded unchanged to System;
// the counters use atomics, allocate no memory, and do not access the allocation.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        // SAFETY: GlobalAlloc's caller supplies the valid nonzero layout unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: this allocator returned ptr through System with the same layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn backend() -> MockBackend {
    let mut backend = MockBackend::new();
    backend
        .set_policy(&X86Policy {
            cpuid: CpuidModel::default(),
            msr_filter: MsrFilter::default(),
        })
        .unwrap();
    backend
}

fn measure(mut op: impl FnMut(), kind: &str, arm: &str, sample: usize, iterations: usize) {
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let bytes = BYTES.load(Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..iterations {
        op();
    }
    let ns = start.elapsed().as_nanos() as f64 / iterations as f64;
    let allocations =
        (ALLOCATIONS.load(Ordering::Relaxed) - allocations) as f64 / iterations as f64;
    let bytes = (BYTES.load(Ordering::Relaxed) - bytes) as f64 / iterations as f64;
    println!(
        "{{\"kind\":\"{kind}\",\"arm\":\"{arm}\",\"sample\":{sample},\"iterations\":{iterations},\"ns\":{ns},\"allocations\":{allocations},\"allocated_bytes\":{bytes}}}"
    );
}

fn vmm(len: usize, check: bool) {
    let mut ram = vmm_core::vmm::GuestRam::new(len).unwrap();
    let mut reference_ram = vmm_core_reference::vmm::GuestRam::new(len).unwrap();
    for (i, byte) in ram.as_mut_bytes().iter_mut().enumerate() {
        *byte = (i.wrapping_mul(37) >> 5) as u8;
    }
    reference_ram.as_mut_bytes().copy_from_slice(ram.as_bytes());
    let memory = ram.as_bytes().to_vec();
    let mut a = vmm_core::vmm::Vmm::new(backend(), ram);
    let mut b = vmm_core_reference::vmm::Vmm::new(backend(), reference_ram);
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
    different.regs.rax ^= 0x1234;
    a.restore_vm_state(&different).unwrap();
    b.restore_vm_state(&different).unwrap();
    assert_ne!(a.state_hash().unwrap(), expected_hash);
    assert_ne!(b.state_hash().unwrap(), expected_hash);
    a.restore_snapshot(&memory, &state).unwrap();
    b.restore_snapshot(&memory, &state).unwrap();
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
                (1024 * 1024 / len).clamp(1, 100)
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
                &format!("hash_{len}"),
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
                &format!("capture_{len}"),
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
                &format!("restore_{len}"),
                name,
                sample,
                iterations,
            );
            assert_eq!(a.state_blob().unwrap(), expected_blob);
            assert_eq!(b.state_blob().unwrap(), expected_blob);
        }
    }
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.iter().any(|x| x == "--allocator-check") {
        let before = ALLOCATIONS.load(Ordering::Relaxed);
        let mut data = Vec::with_capacity(black_box(3));
        data.extend([1_u8, 2, 3]);
        data.reserve(black_box(4096));
        assert_eq!(black_box(&data[..]), [1, 2, 3]);
        drop(data);
        assert!(ALLOCATIONS.load(Ordering::Relaxed) > before);
        return;
    }
    if args.iter().any(|x| x == "--concurrent") {
        let barrier = std::sync::Barrier::new(8);
        let results = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        cached()
                    })
                })
                .collect();
            threads
                .into_iter()
                .map(|t| t.join().unwrap())
                .collect::<Vec<_>>()
        });
        let expected = uncached();
        assert!(results.iter().all(|hash| *hash == expected));
        println!("Concurrent first-use hashes agree");
        return;
    }
    if args.iter().any(|x| x == "--first-use") {
        let arm = args.last().unwrap().as_str();
        let mut value = [0; 32];
        measure(
            || {
                value = if arm == "cached" {
                    cached()
                } else {
                    uncached()
                }
            },
            "first_use",
            arm,
            0,
            1,
        );
        assert_eq!(
            value,
            if arm == "cached" {
                uncached()
            } else {
                cached()
            }
        );
        return;
    }
    let expected = uncached();
    assert_eq!(cached(), expected);
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for _ in 0..1000 {
        assert_eq!(black_box(cached()), expected);
    }
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), before);
    let check = args.iter().any(|x| x == "--check");
    if !check {
        for sample in 0..9 {
            for arm in if sample % 2 == 0 { [0, 1] } else { [1, 0] } {
                measure(
                    || {
                        assert_eq!(
                            black_box(if arm == 0 { uncached() } else { cached() }),
                            expected
                        )
                    },
                    "fingerprint",
                    if arm == 0 { "uncached" } else { "cached" },
                    sample,
                    1000,
                );
            }
        }
    }
    for len in if check {
        vec![4096]
    } else {
        vec![4096, 65536, 1024 * 1024, 128 * 1024 * 1024]
    } {
        vmm(len, check);
    }
    println!("Contract and complete snapshot comparisons passed");
}
