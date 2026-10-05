// SPDX-License-Identifier: AGPL-3.0-or-later
use super::config::{Config, Result};
use faults_workload::FaultAction;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    ops::{Deref, DerefMut},
    path::Path,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    bundle: Option<String>,
    actions: Vec<FaultAction>,
    settle: bool,
    #[serde(default)]
    outcome: serde_json::Value,
}
#[derive(Clone, Debug, Serialize)]
pub struct Manifest {
    #[serde(flatten)]
    pub record: crate::runs::Manifest,
    #[serde(skip)]
    pub config: Config,
    pub bundle: Option<String>,
    pub actions: Vec<FaultAction>,
    pub settle: bool,
    pub uml_host: Option<serde_json::Value>,
}
impl Deref for Manifest {
    type Target = crate::runs::Manifest;
    fn deref(&self) -> &Self::Target {
        &self.record
    }
}
impl DerefMut for Manifest {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.record
    }
}
impl Manifest {
    pub fn new(config: Config, mode: &str, bundle: Option<String>) -> Result<Self> {
        let record = crate::runs::Manifest::new(config.shared()?, mode)?;
        let uml_host =
            (config.backend == crate::config::Backend::Uml).then(|| record.runner_identity.clone());
        Ok(Self {
            record,
            config,
            bundle,
            actions: Vec::new(),
            settle: true,
            uml_host,
        })
    }
    pub fn read(path: &Path) -> Result<Self> {
        let record = crate::runs::Manifest::read(path)?;
        let config = Config::from_shared(&record.config)?;
        let state: State = serde_json::from_value(record.payload.clone())?;
        let uml_host =
            (config.backend == crate::config::Backend::Uml).then(|| record.runner_identity.clone());
        Ok(Self {
            record,
            config,
            bundle: state.bundle,
            actions: state.actions,
            settle: state.settle,
            uml_host,
        })
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut record = self.record.clone();
        record.config = self.config.shared()?;
        record.payload = serde_json::to_value(State {
            bundle: self.bundle.clone(),
            actions: self.actions.clone(),
            settle: self.settle,
            outcome: if path.join("report.json").is_file() {
                serde_json::from_slice(&fs::read(path.join("report.json"))?)?
            } else if path.join("run.json").is_file() {
                serde_json::from_slice(&fs::read(path.join("run.json"))?)?
            } else {
                serde_json::Value::Null
            },
        })?;
        record.save(path)
    }
    pub fn inherit(&self, source: &Path, destination: &Path, mode: &str) -> Result<Self> {
        let mut child = self.clone();
        child.record = self.record.inherit(source, destination, mode)?;
        Ok(child)
    }
    pub fn options(&self, path: &Path) -> faults_workload::Options {
        faults_workload::Options {
            seed: self.config.seed,
            executions: self.config.executions,
            ram_mib: self.config.ram_mib,
            knobs: self.config.knobs.clone(),
            wall_seconds: self.config.wall_seconds,
            output: path.to_path_buf(),
            uml_profile: self.uml_host.as_ref().map(|_| path.join("artifacts/uml")),
        }
    }

    pub fn fault_artifacts(&self, path: &Path) -> Result<faults_workload::Artifacts> {
        Ok(faults_workload::Artifacts {
            kernel: fs::read(path.join("artifacts/kernel"))?,
            initramfs: fs::read(path.join("artifacts/initramfs"))?,
        })
    }
}
