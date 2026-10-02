// SPDX-License-Identifier: AGPL-3.0-or-later

use consonance_client::session::{SearchSession, SparseSnapshot};
use consonance_wasm::{
    Invocation, WasmSession,
    admission::{AdmittedModule, Profile},
};
use control_proto::{StopConditions, StopMask, StopReason, class_bit};
use environment::input_spec::{InputSpec, nominal_factory};
use machine::consonance::ConsonanceMachine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, error::Error, io::Read, path::Path};

const ACTION_FUEL_BUDGET: u64 = 8_000_000_000;

fn package_file(path: &Path, name: &str, limit: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path.join(name))?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(format!("portable NES package file exceeds its bound: {name}").into());
    }
    Ok(bytes)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub quicknes_revision: String,
    pub wasi_sdk: String,
    pub llvm: String,
    pub rustc: String,
    pub entry: String,
    pub memory_pages: u32,
    pub module_sha256: String,
    pub module_bytes: usize,
    pub debug_map_sha256: String,
    pub sources: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct Package {
    manifest: Manifest,
    module: Vec<u8>,
}

impl Package {
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn from_directory(path: &Path) -> Result<Self, Box<dyn Error>> {
        let manifest: Manifest =
            serde_json::from_slice(&package_file(path, "manifest.json", 64 * 1024)?)?;
        let mappings = package_file(path, "debug-map.json", 16 * 1024 * 1024)?;
        if manifest.debug_map_sha256 != format!("{:x}", Sha256::digest(&mappings)) {
            return Err("portable NES debug mapping digest mismatch".into());
        }
        let package = Self::new(
            manifest,
            package_file(
                path,
                "play-agent.wasm",
                Profile::default().maximum_module_bytes as usize,
            )?,
        )?;
        let map: consonance_wasm::source_map::DebugMap = serde_json::from_slice(&mappings)?;
        let admitted = package.admit()?;
        if map.version != 1
            || map.source_digest != admitted.source_digest()
            || map.admitted_digest != <[u8; 32]>::from(Sha256::digest(admitted.bytes()))
            || map.execution_digest != admitted.execution_digest()
        {
            return Err("portable NES debug mapping execution mismatch".into());
        }
        Ok(package)
    }

    pub fn new(manifest: Manifest, module: Vec<u8>) -> Result<Self, Box<dyn Error>> {
        if manifest.version != 1
            || manifest.quicknes_revision != machine::quicknes::QUICKNES_REVISION
            || manifest.wasi_sdk != "27.0"
            || manifest.llvm != "20.1.8"
            || manifest.rustc != "rustc 1.97.0 (2d8144b78 2026-07-07)"
            || manifest.entry != "play"
            || manifest.memory_pages != 256
            || manifest.module_bytes != module.len()
            || manifest.module_sha256 != format!("{:x}", Sha256::digest(&module))
        {
            return Err("portable NES package identity mismatch".into());
        }
        let package = Self { manifest, module };
        package.admit()?;
        Ok(package)
    }

    pub fn admit(&self) -> Result<AdmittedModule, Box<dyn Error>> {
        Ok(AdmittedModule::new(&self.module, Profile::default())?)
    }

    pub fn identity(&self, rom: &[u8]) -> Result<String, Box<dyn Error>> {
        let admitted = self.admit()?;
        let execution: String = admitted
            .execution_digest()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(format!(
            "consonance-wasm-nes-v1;execution={execution};rom={:x};sdk=payload-v1;observation=nes-ram-ring-v2;action-fuel={ACTION_FUEL_BUDGET}",
            Sha256::digest(rom)
        ))
    }

    pub fn session(&self, rom: &[u8], seed: u64) -> Result<WasmSession, Box<dyn Error>> {
        if !(1..=1024 * 1024).contains(&rom.len()) {
            return Err("ROM length outside profile".into());
        }
        let mut input = InputSpec::seeded(seed);
        let mut payloads = vec![(rom.len() as u32).to_le_bytes().to_vec()];
        payloads.extend(rom.chunks(hypercall_proto::MAX_PAYLOAD).map(<[u8]>::to_vec));
        input.set_payloads(Some(payloads));
        let mut session = WasmSession::new(
            self.admit()?,
            input,
            Invocation::new("play"),
            nominal_factory(),
        )?;
        let stop = session.run(
            StopConditions {
                deadline: Some(control_proto::Moment(100_000_000)),
                on: StopMask::NONE.arm(class_bit::SNAPSHOT_POINT),
            },
            None,
        )?;
        if !matches!(stop, StopReason::SnapshotPoint { .. }) {
            return Err(format!(
                "NES setup did not reach its lifecycle event: {stop:?}; console={:?}",
                session.console_tail()?
            )
            .into());
        }
        session.seal_setup()?;
        Ok(session)
    }

    pub fn machine(&self, rom: &[u8], seed: u64) -> Result<ConsonanceMachine, Box<dyn Error>> {
        Ok(ConsonanceMachine::from_session(
            Box::new(self.session(rom, seed)?),
            ACTION_FUEL_BUDGET,
        )?)
    }

    pub fn restored_machine(
        &self,
        snapshot: &SparseSnapshot,
    ) -> Result<ConsonanceMachine, Box<dyn Error>> {
        let session = WasmSession::from_snapshot(self.admit()?, snapshot, nominal_factory())?;
        Ok(ConsonanceMachine::from_restored_session(
            Box::new(session),
            ACTION_FUEL_BUDGET,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nonterminating_action_stops_at_relative_budget_after_replay_and_restore() {
        use consonance_wasm::meter::Meter;
        use hypercall_proto::observation::{Descriptor, EVENT_ID};
        use machine::consonance::BILLBOARD_OBSERVATION_LEN;
        use machine::{Machine, StopConditions as MachineStops, StopReason as MachineStop};

        let mut catalog = b"SDKC\x01".to_vec();
        catalog.extend(2_u32.to_le_bytes());
        for (id, name) in [
            (1_u32, "nes.publication.handle"),
            (2, "nes.publication.len"),
        ] {
            catalog.push(4);
            catalog.extend(id.to_le_bytes());
            catalog.extend((name.len() as u16).to_le_bytes());
            catalog.extend(name.as_bytes());
        }
        let mut events = vec![(0_u32, catalog)];
        for (id, value) in [(1_u32, 1_u64), (2, BILLBOARD_OBSERVATION_LEN as u64)] {
            let mut state = vec![0];
            state.extend(value.to_le_bytes());
            events.push((0x0200_0000 | id, state));
        }
        events.push((
            EVENT_ID,
            Descriptor {
                handle: 1,
                address: 4096,
                len: BILLBOARD_OBSERVATION_LEN as u32,
            }
            .encode()
            .unwrap()
            .to_vec(),
        ));
        events.push((0x0400_0000, vec![]));
        let mut data = String::new();
        let mut calls = String::new();
        for (index, (event_id, body)) in events.into_iter().enumerate() {
            let offset = index * 256;
            let mut bytes = event_id.to_le_bytes().to_vec();
            bytes.extend(body);
            let literal: String = bytes.iter().map(|b| format!("\\{b:02x}")).collect();
            data.push_str(&format!("(data (i32.const {offset}) \"{literal}\")"));
            calls.push_str(&format!("i32.const 262145 i32.const {offset} i32.const {} i32.const 0 i32.const 0 call $request drop ", bytes.len()));
        }
        use machine::consonance::{
            BILLBOARD_MAGIC, BILLBOARD_SAVE_RAM_LEN, BILLBOARD_SAVE_RAM_OFFSET, BILLBOARD_VERSION,
            BILLBOARD_WORK_RAM_LEN, BILLBOARD_WORK_RAM_OFFSET,
        };
        let mut billboard = [0; 32];
        billboard[..4].copy_from_slice(BILLBOARD_MAGIC);
        billboard[4..6].copy_from_slice(&BILLBOARD_VERSION.to_le_bytes());
        for (at, value) in [
            (16, BILLBOARD_WORK_RAM_OFFSET),
            (20, BILLBOARD_WORK_RAM_LEN),
            (24, BILLBOARD_SAVE_RAM_OFFSET),
            (28, BILLBOARD_SAVE_RAM_LEN),
        ] {
            billboard[at..at + 4].copy_from_slice(&(value as u32).to_le_bytes());
        }
        let header: String = billboard[..32]
            .iter()
            .map(|b| format!("\\{b:02x}"))
            .collect();
        data.push_str(&format!("(data (i32.const 4096) \"{header}\")"));
        let source = format!(
            r#"(module
            (import "harmony_v1" "request" (func $request (param i32 i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 256 256)
            {data}
            (func (export "play") {calls} loop br 0 end))"#
        );
        let module =
            AdmittedModule::new(&wat::parse_str(source).unwrap(), Profile::default()).unwrap();
        let mut session = WasmSession::new(
            module.clone(),
            InputSpec::seeded(7),
            Invocation::new("play"),
            nominal_factory(),
        )
        .unwrap();
        assert!(matches!(
            session
                .run(
                    StopConditions {
                        deadline: Some(control_proto::Moment(10000)),
                        on: StopMask::NONE.arm(class_bit::SNAPSHOT_POINT)
                    },
                    None
                )
                .unwrap(),
            StopReason::SnapshotPoint { .. }
        ));
        let (setup, start) = session.seal_setup().unwrap();
        let portable = session.export_sparse_snapshot(setup, None).unwrap();
        let budget = Meter::QUANTUM * 2;
        let mut machine = ConsonanceMachine::from_session(Box::new(session), budget).unwrap();
        let checkpoint = machine.snapshot().unwrap();
        let first = machine.run(MachineStops::default(), None).unwrap();
        assert!(
            matches!(first, MachineStop::Deadline { vtime } if vtime.0 >= start + budget && vtime.0 <= start + budget + Meter::QUANTUM)
        );
        machine.replay(checkpoint).unwrap();
        assert_eq!(machine.run(MachineStops::default(), None).unwrap(), first);
        let restored = WasmSession::from_snapshot(module, &portable, nominal_factory()).unwrap();
        let mut machine =
            ConsonanceMachine::from_restored_session(Box::new(restored), budget).unwrap();
        assert_eq!(machine.run(MachineStops::default(), None).unwrap(), first);
    }

    #[test]
    fn oversized_package_files_fail_before_decoding() {
        for name in ["manifest.json", "debug-map.json", "play-agent.wasm"] {
            let directory = tempfile::tempdir().unwrap();
            let manifest = Manifest {
                version: 0,
                quicknes_revision: String::new(),
                wasi_sdk: String::new(),
                llvm: String::new(),
                rustc: String::new(),
                entry: String::new(),
                memory_pages: 0,
                module_sha256: String::new(),
                module_bytes: 0,
                debug_map_sha256: format!("{:x}", Sha256::digest([])),
                sources: BTreeMap::new(),
            };
            std::fs::write(
                directory.path().join("manifest.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            std::fs::write(directory.path().join("debug-map.json"), []).unwrap();
            std::fs::write(directory.path().join("play-agent.wasm"), []).unwrap();
            std::fs::File::create(directory.path().join(name))
                .unwrap()
                .set_len(1 << 30)
                .unwrap();
            let error = Package::from_directory(directory.path()).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("portable NES package file exceeds its bound: {name}")
            );
        }
    }
}
