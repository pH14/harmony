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
use std::{collections::BTreeMap, error::Error, path::Path};

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
            serde_json::from_slice(&std::fs::read(path.join("manifest.json"))?)?;
        if manifest.debug_map_sha256
            != format!(
                "{:x}",
                Sha256::digest(std::fs::read(path.join("debug-map.json"))?)
            )
        {
            return Err("portable NES debug mapping digest mismatch".into());
        }
        let package = Self::new(manifest, std::fs::read(path.join("play-agent.wasm"))?)?;
        let map: consonance_wasm::source_map::DebugMap =
            serde_json::from_slice(&std::fs::read(path.join("debug-map.json"))?)?;
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
            "consonance-wasm-nes-v1;execution={execution};rom={:x};sdk=payload-v1;observation=nes-ram-ring-v2",
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
        Ok(ConsonanceMachine::from_session(Box::new(
            self.session(rom, seed)?,
        ))?)
    }

    pub fn restored_machine(
        &self,
        snapshot: &SparseSnapshot,
    ) -> Result<ConsonanceMachine, Box<dyn Error>> {
        let session = WasmSession::from_snapshot(self.admit()?, snapshot, nominal_factory())?;
        Ok(ConsonanceMachine::from_restored_session(Box::new(session))?)
    }
}
