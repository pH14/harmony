// SPDX-License-Identifier: AGPL-3.0-or-later

use control_proto::{HashScope, Reply, Reproducer, Request};
use environment::channel::{ChannelError, Question, ServiceHandler, ServiceResponse};
use environment::input_spec::{InputSpec, ServiceConfig};
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;
use vmm_backend::{Backend, MockArm64Backend, MockBackend};

#[derive(Clone, Default)]
struct BlobHandler(Vec<u8>);

impl ServiceHandler for BlobHandler {
    fn identity(&self) -> &[u8] {
        b"qualification.control-capture.v1"
    }
    fn configuration(&self) -> &[u8] {
        &[]
    }
    fn respond(&mut self, _: u64, _: &Question) -> Result<ServiceResponse, ChannelError> {
        Ok(ServiceResponse::External)
    }
    fn snapshot_state(&self) -> Result<Vec<u8>, ChannelError> {
        Ok(self.0.clone())
    }
    fn restore_state(&mut self, state: &[u8]) -> Result<(), ChannelError> {
        self.0 = state.to_vec();
        Ok(())
    }
    fn clone_box(&self) -> Box<dyn ServiceHandler> {
        Box::new(self.clone())
    }
}

macro_rules! define_arm {
    ($name:ident, $core:ident) => {
        fn $name<B: Backend>(
            backend: B,
            label: &str,
            sample: usize,
            check: bool,
            ram_len: usize,
            pattern: &str,
            bytes: usize,
        ) -> ([u8; 32], [u8; 32], Vec<u8>, [u8; 32])
        where
            B::A: $core::vendor::Vendor,
        {
            let ram = $core::vmm::GuestRam::new(ram_len).unwrap();
            let mut vmm = $core::vmm::Vmm::new(backend, ram);
            vmm.wire_vtime($core::vmm::VtimeWiring::new_virtual_time(vtime::VClockConfig {
                guest_hz: 24_000_000, guest_base: 0, vns_base: 0,
            }, 7).unwrap());
            vmm.wire_snapshot_hashing();
            let mut server = $core::control::ControlServer::new(vmm, Box::new(|| panic!("unused factory")));
            server.set_restore_mode($core::control::RestoreMode::InPlace);
            assert!(matches!(server.handle(&Request::Hello($core::control::server_caps())).unwrap().unwrap(), Reply::Hello(_)));
            if pattern != "nominal" {
                let Reply::Snapshot { id: initial, .. } = server.handle(&Request::Snapshot).unwrap().unwrap() else { panic!("initial") };
                let config = ServiceConfig { identity: BlobHandler::default().identity().to_vec(), configuration: vec![] };
                let factory_config = config.clone();
                server.set_service_factory(Arc::new(move |requested| {
                    assert_eq!(requested, &factory_config);
                    Ok(Box::new(BlobHandler::default()))
                }));
                let mut spec = InputSpec::seeded(13);
                spec.set_config(config.clone());
                let data: Vec<u8> = (0..bytes).map(|i| (i.wrapping_mul(37) >> 5) as u8).collect();
                match pattern {
                    "payloads" => spec.set_payloads(Some(data.chunks(256).map(<[u8]>::to_vec).collect())),
                    "answers" => {
                        for (i, chunk) in data.chunks(256).enumerate() {
                            spec.record_answer(0, 19, i as u64, environment::channel::Answer::Data(chunk.to_vec())).unwrap();
                        }
                    }
                    "pending" => {
                        for (i, chunk) in data.chunks(256).enumerate() {
                            spec.record_effect(i as u64 + 1, environment::channel::Effect::WriteMemory { gpa: 8192, bytes: chunk.to_vec() });
                        }
                    }
                    _ => {}
                }
                let env = Reproducer { blob_version: InputSpec::BLOB_VERSION, bytes: spec.encode() };
                assert_eq!(server.handle(&Request::Branch { snap: initial, env }).unwrap().unwrap(), Reply::Unit);
                assert_eq!(server.handle(&Request::Drop(initial)).unwrap().unwrap(), Reply::Unit);
            }
            let hash = server.vmm_mut().unwrap().state_hash().unwrap();
            let Reply::Hash(whole_hash) = server.handle(&Request::Hash { scope: HashScope::Whole }).unwrap().unwrap() else { panic!("whole hash") };
            let Reply::Snapshot { id, at, sdk_events, tainted } = server.handle(&Request::Snapshot).unwrap().unwrap() else { panic!("snapshot") };
            let expected_receipt = (at, sdk_events, tainted);
            let snapshot_hash = server.export_portable_snapshot(id, std::io::sink()).unwrap().state_hash;
            let export = server.export_sparse_snapshot(id, id).unwrap();
            assert!(export.pages.is_empty());
            assert_eq!(server.handle(&Request::Drop(id)).unwrap().unwrap(), Reply::Unit);
            let iterations = if check { 1 } else { (64 * 1024 * 1024 / bytes.max(ram_len)).clamp(1000, 10_000) };
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
            println!("{{\"kind\":\"{label}_{pattern}_{bytes}\",\"arm\":\"{}\",\"sample\":{sample},\"iterations\":{iterations},\"ns\":{ns}}}", stringify!($name));
            (hash, snapshot_hash, export.sidecar, whole_hash)
        }
    };
}

define_arm!(optimized, vmm_core);
define_arm!(reference, vmm_core_reference);

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
        for len in [16 * 1024] {
            for (pattern, bytes) in [
                ("nominal", 0),
                ("payloads", 4096),
                ("payloads", 65536),
                ("payloads", 524288),
                ("answers", 65536),
                ("pending", 65536),
            ] {
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
                                    reference(x86(), backend, sample, check, len, pattern, bytes)
                                } else {
                                    optimized(x86(), backend, sample, check, len, pattern, bytes)
                                }
                            }
                            "arm64" => {
                                if old {
                                    reference(arm64(), backend, sample, check, len, pattern, bytes)
                                } else {
                                    optimized(arm64(), backend, sample, check, len, pattern, bytes)
                                }
                            }
                            _ => {
                                #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
                                {
                                    if old {
                                        reference(hvf(), backend, sample, check, len, pattern, bytes)
                                    } else {
                                        optimized(hvf(), backend, sample, check, len, pattern, bytes)
                                    }
                                }
                                #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
                                {
                                    panic!("HVF requires Apple silicon macOS")
                                }
                            }
                        };
                        if let Some((hash, snapshot_hash, sidecar, whole_hash)) = prior {
                            assert_eq!(evidence.0, hash);
                            assert_eq!(evidence.1, snapshot_hash);
                            assert_eq!(evidence.3, whole_hash);
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
        "Control encoding preserves whole-control and exported snapshot hashes, full VMM hashes, and complete mock sidecars"
    );
}
