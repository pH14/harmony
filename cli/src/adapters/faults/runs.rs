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
    uml_entropy_admitted: bool,
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
    pub uml_entropy_admitted: bool,
    #[serde(skip)]
    expected_outcome: Option<serde_json::Value>,
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
    fn outcome_file(&self) -> &'static str {
        if self.bundle.is_some() {
            "report.json"
        } else {
            "run.json"
        }
    }

    fn outcome(&self, path: &Path) -> Result<serde_json::Value> {
        let file = path.join(self.outcome_file());
        if file.is_file() {
            Ok(serde_json::from_slice(&fs::read(file)?)?)
        } else {
            Ok(serde_json::Value::Null)
        }
    }

    pub fn new(
        config: Config,
        mode: &str,
        bundle: Option<String>,
        image_admitted: bool,
    ) -> Result<Self> {
        let record = crate::runs::Manifest::new(config.shared()?, mode)?;
        let uml_entropy_admitted = image_admitted && config.backend == crate::config::Backend::Uml;
        let uml_host =
            (config.backend == crate::config::Backend::Uml).then(|| record.runner_identity.clone());
        Ok(Self {
            record,
            config,
            bundle,
            actions: Vec::new(),
            settle: true,
            uml_host,
            uml_entropy_admitted,
            expected_outcome: None,
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
            uml_entropy_admitted: state.uml_entropy_admitted,
            expected_outcome: None,
        })
    }
    pub fn pin_outcome(&mut self, outcome: impl Serialize) -> Result<()> {
        self.expected_outcome = Some(serde_json::to_value(outcome)?);
        Ok(())
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        if self.record.status == "complete" && self.expected_outcome.is_none() {
            return Err("completed faults run has no returned outcome".into());
        }
        let outcome = self.outcome(path)?;
        if self
            .expected_outcome
            .as_ref()
            .is_some_and(|expected| expected != &outcome)
        {
            return Err(
                format!("{} differs from the returned outcome", self.outcome_file()).into(),
            );
        }
        let mut record = self.record.clone();
        record.config = self.config.shared()?;
        record.payload = serde_json::to_value(State {
            bundle: self.bundle.clone(),
            actions: self.actions.clone(),
            settle: self.settle,
            uml_entropy_admitted: self.uml_entropy_admitted,
            outcome,
        })?;
        record.save(path)
    }
    pub fn verify(&self, path: &Path) -> Result<()> {
        self.record.verify(path)?;
        if self.outcome(path)? != self.record.payload["outcome"] {
            return Err(format!("recorded {} has changed", self.outcome_file()).into());
        }
        Ok(())
    }
    pub fn inherit(&self, source: &Path, destination: &Path, mode: &str) -> Result<Self> {
        let mut child = self.clone();
        child.record = self.record.inherit(source, destination, mode)?;
        child.expected_outcome = None;
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
            root: self
                .artifacts
                .contains_key("root")
                .then(|| path.join("artifacts/root")),
            uml_profile: self.uml_host.as_ref().map(|_| path.join("artifacts/uml")),
            checkpoint_owner: self.record.checkpoint_owner.clone(),
        }
    }

    pub fn fault_artifacts(&self, path: &Path) -> Result<faults_workload::Artifacts> {
        if self.config.backend == crate::config::Backend::Uml && !self.uml_entropy_admitted {
            return Err("this UML run predates hardware-entropy image admission and cannot be replayed safely".into());
        }
        Ok(faults_workload::Artifacts {
            kernel: fs::read(path.join("artifacts/kernel"))?,
            initramfs: fs::read(path.join("artifacts/initramfs"))?,
        })
    }
}

pub fn semantic_outcome(payload: &serde_json::Value) -> serde_json::Value {
    let mut outcome = payload.clone();
    if let Some(report) = outcome
        .get_mut("outcome")
        .and_then(serde_json::Value::as_object_mut)
    {
        report.remove("wall_seconds");
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edited_report_fails_replay_verification_but_partial_report_allows_resume() {
        let dir = tempfile::tempdir().unwrap();
        let report = serde_json::json!({
            "package":"faults","mode":"search","image_sha256":"image","kernel_sha256":"kernel",
            "identity":"runner","seed":0,"workers":1,"ram_mib":1024,"executions":1,
            "bug_found":false,"first_bug_execution":null,"bugs":[],"replays":[],
            "execution_ticks":1,"wall_seconds":1,"execution_failures":0
        });
        fs::write(
            dir.path().join("report.json"),
            serde_json::to_vec(&report).unwrap(),
        )
        .unwrap();
        let mut shared = crate::config::Config::default();
        shared.runner.kind = "test".into();
        shared.workload.package = "faults".into();
        let mut record = crate::runs::Manifest::new(shared, "search").unwrap();
        record.payload = serde_json::json!({
            "bundle":"node app \"/bin/app\"\n","actions":[],"settle":true,"outcome":report
        });
        record.save(dir.path()).unwrap();
        let manifest = Manifest::read(dir.path()).unwrap();
        manifest.verify(dir.path()).unwrap();

        let mut changed = report;
        changed["bug_found"] = true.into();
        fs::write(
            dir.path().join("report.json"),
            serde_json::to_vec(&changed).unwrap(),
        )
        .unwrap();
        let read: faults_workload::Report =
            serde_json::from_slice(&fs::read(dir.path().join("report.json")).unwrap()).unwrap();
        assert!(read.bug_found);
        assert!(
            manifest
                .verify(dir.path())
                .unwrap_err()
                .to_string()
                .contains("report.json has changed")
        );

        fs::write(dir.path().join("report.json"), b"{\"bugs\":[").unwrap();
        let resume = crate::adapters::Request {
            operation: crate::adapters::Operation::Resume,
            selection: Some(crate::selection::Selection {
                run: dir.path().display().to_string(),
                finding: None,
            }),
            ..Default::default()
        };
        assert!(
            super::super::workflow::continue_search(resume)
                .unwrap_err()
                .to_string()
                .contains("no whole-search checkpoint yet")
        );
    }

    #[test]
    fn edited_command_record_fails_verification_before_replay() {
        let dir = tempfile::tempdir().unwrap();
        let recorded = serde_json::json!({"exit_code":0,"output":"same"});
        fs::write(
            dir.path().join("run.json"),
            serde_json::to_vec(&recorded).unwrap(),
        )
        .unwrap();
        let mut shared = crate::config::Config::default();
        shared.runner.kind = "test".into();
        shared.workload.package = "faults".into();
        let mut record = crate::runs::Manifest::new(shared, "run").unwrap();
        record.payload = serde_json::json!({
            "bundle":null,"actions":[],"settle":true,"outcome":recorded
        });
        record.save(dir.path()).unwrap();
        let manifest = Manifest::read(dir.path()).unwrap();
        manifest.verify(dir.path()).unwrap();

        fs::write(
            dir.path().join("run.json"),
            serde_json::to_vec(&serde_json::json!({"exit_code":0,"output":"changed"})).unwrap(),
        )
        .unwrap();
        assert!(
            manifest
                .verify(dir.path())
                .unwrap_err()
                .to_string()
                .contains("run.json has changed")
        );
        fs::remove_file(dir.path().join("run.json")).unwrap();
        assert!(manifest.verify(dir.path()).is_err());
    }

    #[test]
    fn old_run_state_has_no_uml_entropy_admission_evidence() {
        let state: State = serde_json::from_value(serde_json::json!({
            "bundle": null,
            "actions": [],
            "settle": true
        }))
        .unwrap();
        assert!(!state.uml_entropy_admitted);
    }

    #[test]
    fn semantic_comparison_ignores_host_duration_but_keeps_execution_changes() {
        let a = serde_json::json!({"actions":[1],"outcome":{"wall_seconds":1,"replays":[{"state_hash":"same"}]}});
        let mut b = a.clone();
        b["outcome"]["wall_seconds"] = 2.into();
        assert_eq!(semantic_outcome(&a), semantic_outcome(&b));
        b["outcome"]["replays"][0]["state_hash"] = "changed".into();
        assert_ne!(semantic_outcome(&a), semantic_outcome(&b));
    }
}
