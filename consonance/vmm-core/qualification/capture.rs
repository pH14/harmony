// SPDX-License-Identifier: AGPL-3.0-or-later

use control_proto::{Reply, Request};
use std::hint::black_box;
use std::time::Instant;
use vm_state::SnapshotRecords;
use vmm_backend::{Backend, MockArm64Backend, MockBackend};

macro_rules! define_arm {
    ($name:ident, $core:ident) => {
        fn $name<B: Backend>(
            backend: B,
            label: &str,
            sample: usize,
            check: bool,
            ram_len: usize,
            pattern: &str,
        ) -> ([u8; 32], [u8; 32], Vec<u8>)
        where
            B::A: $core::vendor::Vendor,
        {
            let mut ram = $core::vmm::GuestRam::new(ram_len).unwrap();
            for (gfn, page) in ram.as_mut_bytes().chunks_mut(4096).enumerate() {
                if pattern == "dense" || (pattern == "sparse" && gfn % 16 == 0) {
                    for (i, byte) in page.iter_mut().enumerate() {
                        *byte = (gfn.wrapping_mul(73) ^ i.wrapping_mul(37)) as u8;
                    }
                }
            }
            let mut vmm = $core::vmm::Vmm::new(backend, ram);
            vmm.wire_snapshot_hashing();
            if ram_len == 16384 && pattern == "zero" {
                let expected = vmm.save_vm_state().unwrap().encode_for_hash().unwrap();
                let iterations = if check { 1 } else { 100_000 };
                let start = Instant::now();
                for _ in 0..iterations {
                    assert_eq!(black_box(vmm.save_vm_state().unwrap().encode_for_hash().unwrap()), expected);
                }
                let ns = start.elapsed().as_nanos() as f64 / iterations as f64;
                println!("{{\"kind\":\"public_save_{label}\",\"arm\":\"{}\",\"sample\":{sample},\"iterations\":{iterations},\"ns\":{ns}}}", stringify!($name));
            }
            let mut server = $core::control::ControlServer::new(vmm, Box::new(|| panic!("unused factory")));
            assert!(matches!(server.handle(&Request::Hello($core::control::server_caps())).unwrap().unwrap(), Reply::Hello(_)));
            let hash = server.vmm_mut().unwrap().state_hash().unwrap();
            let Reply::Snapshot { id, at, sdk_events, tainted } = server.handle(&Request::Snapshot).unwrap().unwrap() else { panic!("snapshot") };
            let expected_receipt = (at, sdk_events, tainted);
            let snapshot_hash = server.export_portable_snapshot(id, std::io::sink()).unwrap().state_hash;
            let export = server.export_sparse_snapshot(id, id).unwrap();
            assert!(export.pages.is_empty());
            assert_eq!(server.handle(&Request::Drop(id)).unwrap().unwrap(), Reply::Unit);
            let iterations = if check { 1 } else { (160 * 1024 * 1024 / ram_len).clamp(1000, 10_000) };
            let mut snapshot = || {
                let Reply::Snapshot { id, at, sdk_events, tainted } = server.handle(black_box(&Request::Snapshot)).unwrap().unwrap() else { panic!("snapshot") };
                assert_eq!((at, sdk_events, tainted), expected_receipt);
                assert_eq!(server.handle(&Request::Drop(id)).unwrap().unwrap(), Reply::Unit);
            };
            snapshot();
            let start = Instant::now();
            for _ in 0..iterations { snapshot(); }
            let ns = start.elapsed().as_nanos() as f64 / iterations as f64;
            assert_eq!(server.vmm_mut().unwrap().state_hash().unwrap(), hash);
            assert_eq!(server.snapshot_store_stats().snapshots, 0);
            println!("{{\"kind\":\"{label}_{ram_len}_{pattern}\",\"arm\":\"{}\",\"sample\":{sample},\"iterations\":{iterations},\"ns\":{ns}}}", stringify!($name));
            (hash, snapshot_hash, export.sidecar)
        }
    };
}

define_arm!(shared, vmm_core);
define_arm!(repeated, vmm_core_reference);

fn x86() -> MockBackend {
    let mut backend = MockBackend::new();
    backend
        .set_policy(&vmm_backend::X86Policy {
            cpuid: Default::default(),
            msr_filter: Default::default(),
        })
        .unwrap();
    let mut state = backend.save().unwrap();
    state.regs.rax = 0x1234;
    state.regs.rip = 0x5678;
    state.xsave = vec![0; 4096];
    state.xsave[..2].copy_from_slice(&0x037fu16.to_le_bytes());
    state.xsave[24..28].copy_from_slice(&0x1f80u32.to_le_bytes());
    state.xsave[512..520].copy_from_slice(&7u64.to_le_bytes());
    state.xsave[576] = 0x47;
    backend.restore(&state).unwrap();
    backend
}

fn arm64() -> MockArm64Backend {
    let mut backend = MockArm64Backend::new();
    let width = backend.capabilities().arch.asid_bits;
    backend
        .set_policy(&vmm_core::vendor::arm64::contract::policy(width))
        .unwrap();
    let mut state = backend.save().unwrap();
    state.core.x[0] = 0x1234;
    state.core.pc = 0x5678;
    state.simd_fp.q[7] = [0x47; 16];
    state.vtimer.counter = 0x1234_5678_90ab_cdef;
    backend.restore(&state).unwrap();
    backend
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn hvf() -> vmm_backend::HvfBackend {
    let mut backend = vmm_backend::HvfBackend::new().unwrap();
    let width = backend.capabilities().arch.asid_bits;
    backend
        .set_policy(&vmm_core::vendor::arm64::contract::policy(width))
        .unwrap();
    backend
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.iter().any(|s| s == "--identity-only") {
        return;
    }
    let check = args.iter().any(|s| s == "--check");
    let live = args.iter().any(|s| s == "--hvf");
    let backends = if live {
        vec!["hvf"]
    } else {
        vec!["x86", "arm64"]
    };
    for backend in backends {
        for len in [16 * 1024, 1024 * 1024] {
            for pattern in ["zero", "sparse", "dense"] {
                for sample in 0..if check { 1 } else { 9 } {
                    let mut prior = None;
                    for old in if sample % 2 == 0 {
                        [true, false]
                    } else {
                        [false, true]
                    } {
                        let evidence = match backend {
                            "x86" => {
                                if old {
                                    repeated(x86(), backend, sample, check, len, pattern)
                                } else {
                                    shared(x86(), backend, sample, check, len, pattern)
                                }
                            }
                            "arm64" => {
                                if old {
                                    repeated(arm64(), backend, sample, check, len, pattern)
                                } else {
                                    shared(arm64(), backend, sample, check, len, pattern)
                                }
                            }
                            _ => {
                                #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
                                {
                                    if old {
                                        repeated(hvf(), backend, sample, check, len, pattern)
                                    } else {
                                        shared(hvf(), backend, sample, check, len, pattern)
                                    }
                                }
                                #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
                                {
                                    panic!("HVF requires Apple silicon macOS")
                                }
                            }
                        };
                        if let Some((hash, snapshot_hash, sidecar)) = prior {
                            assert_eq!(evidence.0, hash);
                            assert_eq!(evidence.1, snapshot_hash);
                            if !live {
                                assert_eq!(evidence.2, sidecar);
                            }
                        }
                        prior = Some(evidence);
                    }
                }
            }
        }
    }
    println!(
        "Shared capture preserves exported snapshot hashes, full VMM hashes, and complete mock sidecars"
    );
}
