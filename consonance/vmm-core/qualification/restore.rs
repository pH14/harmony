// SPDX-License-Identifier: AGPL-3.0-or-later

use control_proto::{Reply, Reproducer, Request};
use environment::channel::{ChannelError, Question, RecordedEnv, ServiceHandler, ServiceResponse};
use environment::input_spec::{InputSpec, ServiceConfig};
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;
use vmm_backend::{Backend, MockArm64Backend, MockBackend};

#[derive(Clone, Default)]
struct BlobHandler(Vec<u8>);

impl ServiceHandler for BlobHandler {
    fn identity(&self) -> &[u8] {
        b"qualification.restore.v1"
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

#[derive(Clone, Copy)]
struct Case {
    bytes: usize,
    branch: bool,
    in_place: bool,
    history: bool,
}

macro_rules! define_arm {
    ($name:ident, $core:ident) => {
        mod $name {
            use super::*;
            use $core::control::{ControlServer, RestoreMode, server_caps};
            use $core::vmm::{GuestRam, Vmm, VtimeWiring};

            fn machine<B: Backend>(backend: fn() -> B) -> Vmm<B>
            where B::A: $core::vendor::Vendor {
                let mut ram = GuestRam::new(16384).unwrap();
                ram.as_mut_bytes()[8192] = 0x47;
                let mut vmm = Vmm::new(backend(), ram);
                vmm.wire_vtime(VtimeWiring::new_virtual_time(vtime::VClockConfig {
                    guest_hz: 24_000_000, guest_base: 0, vns_base: 0,
                }, 7).unwrap());
                vmm.wire_snapshot_hashing();
                vmm
            }

            pub fn run<B: Backend + 'static>(backend: fn() -> B, label: &str, case: Case, check: bool, sample: usize) -> Vec<u8>
            where B::A: $core::vendor::Vendor {
                let Case { bytes, branch, in_place, history } = case;
                let mut server = ControlServer::new(machine(backend), Box::new(move || Ok(machine(backend))));
                server.set_restore_mode(if in_place { RestoreMode::InPlace } else { RestoreMode::Memcpy });
                assert!(matches!(server.handle(&Request::Hello(server_caps())).unwrap().unwrap(), Reply::Hello(_)));
                let Reply::Snapshot { id: initial, .. } = server.handle(&Request::Snapshot).unwrap().unwrap() else { panic!("snapshot") };
                let config = ServiceConfig { identity: BlobHandler::default().identity().to_vec(), configuration: vec![] };
                let factory_config = config.clone();
                server.set_service_factory(Arc::new(move |requested| {
                    assert_eq!(requested, &factory_config);
                    Ok(Box::new(BlobHandler::default()))
                }));
                let mut spec = InputSpec::seeded(13);
                spec.set_config(config.clone());
                let env = Reproducer { blob_version: InputSpec::BLOB_VERSION, bytes: spec.encode() };
                assert_eq!(server.handle(&Request::Branch { snap: initial, env: env.clone() }).unwrap().unwrap(), Reply::Unit);
                let data: Vec<u8> = (0..bytes).map(|i| (i.wrapping_mul(37) >> 5) as u8).collect();
                let mut recorded = RecordedEnv::new(13, Box::new(BlobHandler(if history { Vec::new() } else { data.clone() })) as Box<dyn ServiceHandler>);
                if history {
                    recorded.set_payloads(Some(data.chunks(256).map(<[u8]>::to_vec).collect())).unwrap();
                    for (i, chunk) in data.chunks(256).enumerate() {
                        recorded.record_service_request(0, 19, i as u64, environment::channel::Answer::Data(chunk.to_vec()));
                    }
                }
                server.vmm_mut().unwrap().enable_sdk(recorded, &config);
                let Reply::Snapshot { id, .. } = server.handle(&Request::Snapshot).unwrap().unwrap() else { panic!("snapshot") };
                assert_eq!(server.handle(&Request::Drop(initial)).unwrap().unwrap(), Reply::Unit);
                let original = server.export_sparse_snapshot(id, id).unwrap().sidecar;
                let request = if branch { Request::Branch { snap: id, env } } else { Request::Replay(id) };
                let iterations = if check { 2 } else if !in_place { 500 } else { (64 * 1024 * 1024 / bytes.max(4096)).clamp(1000, 10_000) };
                assert_eq!(server.handle(&request).unwrap().unwrap(), Reply::Unit);
                let start = Instant::now();
                for _ in 0..iterations {
                    assert_eq!(server.handle(black_box(&request)).unwrap().unwrap(), Reply::Unit);
                }
                let ns = start.elapsed().as_nanos() as f64 / iterations as f64;
                assert_eq!(server.in_place_fallbacks(), 0);
                assert_eq!(server.export_sparse_snapshot(id, id).unwrap().sidecar, original);
                let Reply::Snapshot { id: restored, .. } = server.handle(&Request::Snapshot).unwrap().unwrap() else { panic!("snapshot") };
                let output = server.export_sparse_snapshot(restored, restored).unwrap().sidecar;
                if !branch { assert_eq!(output, original); }
                let mode = if branch { "branch" } else { "replay" };
                let memory = if in_place { "in_place" } else { "memcpy" };
                let profile = if history { "history" } else { "handler" };
                println!("{{\"kind\":\"{label}_{memory}_{mode}_{profile}_{bytes}\",\"arm\":\"{}\",\"sample\":{sample},\"iterations\":{iterations},\"ns\":{ns}}}", stringify!($name));
                output
            }
        }
    }
}

define_arm!(borrowed, vmm_core);
define_arm!(cloned, vmm_core_reference);

fn x86() -> MockBackend {
    let mut b = MockBackend::new();
    b.set_policy(&vmm_backend::X86Policy {
        cpuid: Default::default(),
        msr_filter: Default::default(),
    })
    .unwrap();
    b
}

fn arm64() -> MockArm64Backend {
    let mut b = MockArm64Backend::new();
    b.set_policy(&vmm_core::vendor::arm64::contract::policy(
        b.capabilities().arch.asid_bits,
    ))
    .unwrap();
    b
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let check = args.iter().any(|a| a == "--check");
    let filter = args
        .windows(2)
        .find(|pair| pair[0] == "--filter")
        .map(|pair| pair[1].as_str());
    let mut cases = 0;
    for arch in ["x86", "arm64"] {
        for (bytes, history) in [
            (0, false),
            (4096, false),
            (65536, false),
            (1048576, false),
            (65536, true),
        ] {
            for in_place in [true, false] {
                for branch in [false, true] {
                    let case = Case {
                        bytes,
                        history,
                        in_place,
                        branch,
                    };
                    let memory = if in_place { "in_place" } else { "memcpy" };
                    let mode = if branch { "branch" } else { "replay" };
                    let profile = if history { "history" } else { "handler" };
                    let name = format!("{arch}_{memory}_{mode}_{profile}_{bytes}");
                    if filter.is_some_and(|f| !name.contains(f)) {
                        continue;
                    }
                    cases += 1;
                    for sample in 0..if check { 1 } else { 9 } {
                        let mut prior = None;
                        for old in if sample % 2 == 0 {
                            [true, false]
                        } else {
                            [false, true]
                        } {
                            let output = match (arch, old) {
                                ("x86", true) => cloned::run(x86, arch, case, check, sample),
                                ("x86", false) => borrowed::run(x86, arch, case, check, sample),
                                (_, true) => cloned::run(arm64, arch, case, check, sample),
                                (_, false) => borrowed::run(arm64, arch, case, check, sample),
                            };
                            if let Some(previous) = prior {
                                assert_eq!(output, previous);
                            }
                            prior = Some(output);
                        }
                    }
                }
            }
        }
    }
    assert!(cases > 0, "no qualification cases matched");
    println!("Borrowed SDK restores preserve complete sidecars and reusable source snapshots");
}
