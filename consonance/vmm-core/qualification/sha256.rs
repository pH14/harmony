// SPDX-License-Identifier: AGPL-3.0-or-later

use sha2::Digest;
use std::hint::black_box;
use std::time::Instant;
use vmm_backend::Backend;
use vmm_backend::MockBackend;
use vmm_backend::arch::x86::{CpuidModel, MsrFilter, X86Policy};
use vmm_core::vmm::{GuestRam, Vmm};

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

fn bytes(len: usize) -> Vec<u8> {
    let mut seed = 0x123456789abcdef_u64;
    (0..len)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        })
        .collect()
}

fn check_hashes() {
    let data = bytes(8193);
    for len in (0..=257).chain([4095, 4096, 4097, 8192, 8193]) {
        for offset in 0..=3 {
            let input = &data[offset..offset + len.min(data.len() - offset)];
            for chunk in [1, 7, 55, 56, 63, 64, 65, 4096] {
                let mut native = sha2::Sha256::new();
                let mut software = sha2_reference::Sha256::new();
                for block in input.chunks(chunk) {
                    native.update(block);
                    software.update(block);
                }
                assert_eq!(native.clone().finalize(), software.clone().finalize());
                native.update(b"suffix");
                software.update(b"suffix");
                assert_eq!(native.finalize(), software.finalize());
            }
        }
    }
}

fn qualify_vmm(len: usize, nonzero: bool, check_only: bool) {
    let mut ram = GuestRam::new(len).unwrap();
    let mut reference_ram = vmm_core_reference::vmm::GuestRam::new(len).unwrap();
    if nonzero {
        let contents = bytes(len);
        ram.as_mut_bytes().copy_from_slice(&contents);
        reference_ram.as_mut_bytes().copy_from_slice(&contents);
    }
    let mut native = Vmm::new(backend(), ram);
    let mut software = vmm_core_reference::vmm::Vmm::new(backend(), reference_ram);
    native.wire_snapshot_hashing();
    software.wire_snapshot_hashing();
    let native_blob = native.state_blob().unwrap();
    assert_eq!(native_blob, software.state_blob().unwrap());
    let expected: [u8; 32] = sha2_reference::Sha256::digest(&native_blob).into();
    drop(native_blob);
    assert_eq!(native.state_hash().unwrap(), expected);
    assert_eq!(software.state_hash().unwrap(), expected);
    if check_only {
        return;
    }
    let iterations = (16 * 1024 * 1024 / len).clamp(1, 1000);
    for sample in 0..9 {
        for arm in if sample % 2 == 0 { [0, 1] } else { [1, 0] } {
            let start = Instant::now();
            for _ in 0..iterations {
                let value = if arm == 0 {
                    black_box(&mut software).state_hash().unwrap()
                } else {
                    black_box(&mut native).state_hash().unwrap()
                };
                assert_eq!(black_box(value), expected);
            }
            println!(
                "{{\"kind\":\"vmm_state_hash\",\"bytes\":{len},\"nonzero\":{nonzero},\"sample\":{sample},\"arm\":\"{}\",\"iterations\":{iterations},\"ns_per_hash\":{}}}",
                if arm == 0 { "software" } else { "native" },
                start.elapsed().as_nanos() as f64 / iterations as f64,
            );
        }
    }
}

fn qualify_digest(len: usize) {
    let data = bytes(len);
    let expected = sha2_reference::Sha256::digest(&data);
    assert_eq!(sha2::Sha256::digest(&data), expected);
    let iterations = (1024 * 1024 / len.max(1)).clamp(1, 20000);
    for sample in 0..9 {
        for arm in if sample % 2 == 0 { [0, 1] } else { [1, 0] } {
            let start = Instant::now();
            for _ in 0..iterations {
                let value = if arm == 0 {
                    sha2_reference::Sha256::digest(black_box(&data))
                } else {
                    sha2::Sha256::digest(black_box(&data))
                };
                assert_eq!(black_box(value), expected);
            }
            println!(
                "{{\"kind\":\"digest\",\"bytes\":{len},\"sample\":{sample},\"arm\":\"{}\",\"iterations\":{iterations},\"ns_per_hash\":{}}}",
                if arm == 0 { "software" } else { "native" },
                start.elapsed().as_nanos() as f64 / iterations as f64,
            );
        }
    }
}

fn main() {
    let check_only = std::env::args().any(|arg| arg == "--check");
    check_hashes();
    if !check_only {
        for len in [0, 1, 32, 55, 56, 64, 65, 4096, 1024 * 1024] {
            qualify_digest(len);
        }
    }
    let sizes: &[usize] = if check_only {
        &[4096, 65536]
    } else {
        &[4096, 65536, 1024 * 1024, 128 * 1024 * 1024]
    };
    for &len in sizes {
        for nonzero in [false, true] {
            qualify_vmm(len, nonzero, check_only);
        }
    }
    println!("SHA-256 and whole-state comparisons passed");
}
