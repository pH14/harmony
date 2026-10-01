// SPDX-License-Identifier: AGPL-3.0-or-later

use consonance_client::{
    catalog::StateCatalog,
    session::{Session, SessionConfig, SparseSnapshot},
};
use control_proto::SnapId;
use harmony_sdk::wire;
use searcher::search::{
    archive::{ArchiveEntryReport, ArchiveKey, Input, RetentionPolicy},
    campaign::{
        self, ArchiveReportState, CampaignActionResult, CampaignConfig, CampaignJobResult,
        CampaignOrigin, CampaignTypes, Evaluation, InputPolicy, Reporting, TargetExecution,
        WorkloadPolicies,
    },
    draw::{DrawMixture, SuffixShape},
    rand::RomuDuoJrRand,
    rollout::ExecutionDisposition,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs,
    io::BufWriter,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct Options {
    pub seed: u64,
    pub executions: u64,
    pub ram_mib: u32,
    pub wall_minutes: Option<u64>,
    pub output: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Failure {
    pub layer: String,
    pub assertion: Option<u32>,
    pub detail: String,
    pub actions: Vec<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Observation {
    pub registers: [u64; 11],
    pub failure: Option<Failure>,
}

impl Observation {
    fn disposition(&self) -> ExecutionDisposition {
        if self.failure.is_some() {
            ExecutionDisposition::Failed
        } else {
            ExecutionDisposition::Runnable
        }
    }

    fn prepare_restore(&mut self, saved: &Self) -> bool {
        if self.failure.is_some() {
            return false;
        }
        self.clone_from(saved);
        true
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Snapshot {
    state: SparseSnapshot,
    at: u64,
    observation: Observation,
    actions: Vec<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Key([u64; 3]);

impl ArchiveKey for Key {
    type Place = [u64; 3];
    type Progress = ();
    type Identity = ();
    type Lineage = ();
    fn place(self) -> Self::Place {
        self.0
    }
    fn progress(self) -> Self::Progress {}
    fn identity(self) -> Self::Identity {}
    fn complete(self, _: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }
    fn record(_: &mut Self::Lineage, _: Self) {}
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Evidence {
    pub pairs: u64,
    pub failures: Vec<Failure>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Report {
    pub executions: u64,
    pub entries: Vec<ArchiveEntryReport<u64, Key, u64>>,
    pub evidence: Evidence,
}

fn read_observation(session: &mut Session) -> Result<Observation, Box<dyn Error>> {
    let mut catalog = StateCatalog::default();
    for (_, id, bytes) in session.sdk_events()? {
        catalog.observe(id, &bytes)?;
    }
    let mut registers = [0; 11];
    for (index, name) in [
        "creations",
        "imports",
        "steps",
        "bytes.0",
        "bytes.1",
        "bytes.2",
        "snapshots",
        "fork_depth",
        "operations",
        "operation",
        "pairs",
    ]
    .into_iter()
    .enumerate()
    {
        registers[index] = catalog.get(&format!("nested.{name}"))?;
    }
    if registers[0] != 1 {
        return Err("inner VM creation count differs from one".into());
    }
    Ok(Observation {
        registers,
        failure: None,
    })
}

pub struct Target {
    session: Session,
    root: Snapshot,
    current: SnapId,
    at: u64,
    actions: Vec<u64>,
    observation: Observation,
    work: u64,
    export_base: Option<SparseSnapshot>,
}

impl Target {
    fn new(workload: &NestedWorkload) -> Result<Self, Box<dyn Error>> {
        let mut session = Session::new_with_config(
            &workload.kernel,
            &workload.initramfs,
            workload.config.clone(),
        )?;
        let observation = read_observation(&mut session)?;
        let (current, at) = session.setup_handle();
        let state = session.setup_sparse_snapshot()?;
        let root = Snapshot {
            state: state.clone(),
            at,
            observation: observation.clone(),
            actions: Vec::new(),
        };
        Ok(Self {
            session,
            root,
            current,
            at,
            actions: Vec::new(),
            observation,
            work: 0,
            export_base: Some(state),
        })
    }

    fn fail(&mut self, layer: &str, error: impl std::fmt::Display) {
        let detail = error.to_string();
        let assertion = self.session.sdk_events().ok().and_then(|events| {
            events.into_iter().rev().find_map(|(_, id, bytes)| {
                let (namespace, point) = wire::split(id);
                (namespace == wire::NS_ASSERT && bytes.first() == Some(&wire::DISP_VIOLATION))
                    .then_some(point)
            })
        });
        let console = self.session.console_tail().unwrap_or_default();
        let console = String::from_utf8_lossy(&console);
        let level = match assertion {
            Some(crate::operations::ORACLE_ASSERTION) => "L2",
            Some(_) => "inner-VMM",
            None if console.contains("kernel BUG") || console.contains("Kernel panic") => "L1",
            None => layer,
        };
        if let Ok(observation) = read_observation(&mut self.session) {
            self.observation = observation;
        }
        self.observation.failure = Some(Failure {
            layer: level.into(),
            assertion,
            detail: format!(
                "{detail}\n{}",
                &console[console.floor_char_boundary(console.len().saturating_sub(4096))..]
            ),
            actions: self.actions.clone(),
        });
    }

    fn replace_current(&mut self, next: SnapId, at: u64) -> Result<(), Box<dyn Error>> {
        let old = self.current;
        self.current = next;
        self.at = at;
        if old != self.session.setup_handle().0 {
            self.session.drop_snapshot(old)?;
        }
        Ok(())
    }

    fn apply(&mut self, seed: u64) {
        if self.observation.failure.is_some() {
            return;
        }
        self.actions.push(seed);
        self.work += 1;
        let result = (|| {
            self.session.branch_with_seed(self.current, seed)?;
            self.session.run_to_snapshot(self.at)?;
            let observation = read_observation(&mut self.session)?;
            let (next, at) = self.session.snapshot()?;
            self.replace_current(next, at)?;
            self.observation = observation;
            Ok::<_, Box<dyn Error>>(())
        })();
        if let Err(error) = result {
            self.fail("outer-VMM", error);
        }
    }

    fn capture(&mut self) -> Result<Snapshot, Box<dyn Error>> {
        let state = self
            .session
            .export_sparse_snapshot(self.current, self.export_base.as_ref())?;
        self.export_base = Some(state.clone());
        Ok(Snapshot {
            state,
            at: self.at,
            observation: self.observation.clone(),
            actions: self.actions.clone(),
        })
    }

    fn restore(&mut self, snapshot: &Snapshot) {
        if !self.observation.prepare_restore(&snapshot.observation) {
            return;
        }
        self.actions.clone_from(&snapshot.actions);
        let result = (|| {
            let next = self.session.import_sparse_snapshot(&snapshot.state)?;
            self.session.replay_snapshot(next)?;
            let observed = read_observation(&mut self.session)?;
            if observed != snapshot.observation {
                return Err("outer restore changed the inner counts or published state".into());
            }
            self.replace_current(next, snapshot.at)?;
            self.export_base = Some(snapshot.state.clone());
            Ok::<_, Box<dyn Error>>(())
        })();
        if let Err(error) = result {
            self.fail("outer-VMM", error);
        }
    }
}

pub struct NestedWorkload {
    kernel: Vec<u8>,
    initramfs: Vec<u8>,
    config: SessionConfig,
    identity: String,
}

impl NestedWorkload {
    fn new(kernel: &[u8], initramfs: &[u8], options: &Options) -> Self {
        let config = SessionConfig {
            ram_bytes: options.ram_mib as usize * (1 << 20),
            seed: options.seed,
            ..SessionConfig::default()
        }
        .with_nested_host()
        .with_deferred_virtual_time_checkpoint_hashes()
        .with_wall_limit(Duration::from_secs(20));
        Self {
            kernel: kernel.to_vec(),
            initramfs: initramfs.to_vec(),
            identity: Session::identity_with_config(kernel, initramfs, &config),
            config,
        }
    }
}

impl CampaignTypes for NestedWorkload {
    type Target = Target;
    type Action = u64;
    type Key = Key;
    type Milestones = u64;
    type Progress = u32;
    type Snapshot = Snapshot;
    type Observations = Observation;
    type Evidence = Evidence;
    type ArchiveReport = Report;
    type Run = ();
}

impl InputPolicy for NestedWorkload {
    fn max_action_cost(&self) -> u64 {
        1
    }
    fn policies(&self, _: &()) -> WorkloadPolicies {
        [
            ("image", self.identity.as_str()),
            ("action_format", "sdk-entropy-seed-u64-v1"),
            ("key_policy", "snapshot-count-fork-depth-operation-v1"),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect()
    }
    fn resolve_recorded(&self, policies: &WorkloadPolicies) -> Result<(), Box<dyn Error>> {
        if policies != &self.policies(&()) {
            return Err("nested workload image or recorded policy differs".into());
        }
        Ok(())
    }
    fn sample_alphabet(&self, _: &(), rand: &mut RomuDuoJrRand) -> Result<u64, Box<dyn Error>> {
        Ok(rand.next_u64())
    }
}

impl TargetExecution for NestedWorkload {
    fn new_target(&self) -> Result<Target, String> {
        Target::new(self).map_err(|error| error.to_string())
    }
    fn reset(&self, target: &mut Target) {
        target.observation.failure = None;
        target.restore(&target.root.clone());
    }
    fn restore(&self, target: &mut Target, snapshot: &Snapshot) -> Result<(), Box<dyn Error>> {
        target.restore(snapshot);
        Ok(())
    }
    fn execution_work(&self, target: &Target) -> u64 {
        target.work
    }
    fn action_cost_fn(&self) -> fn(&u64) -> u64 {
        |_| 1
    }
    fn snapshot_memory_charge(snapshot: &Snapshot) -> usize {
        snapshot.state.memory_charge() + snapshot.actions.len() * 8
    }
    fn apply_action(
        &self,
        target: &mut Target,
        action: &u64,
        milestones: &mut u64,
    ) -> Result<(), Box<dyn Error>> {
        target.apply(*action);
        *milestones |= target.observation.registers[10];
        Ok(())
    }
    fn rollout_observations(&self, target: &Target) -> Vec<Observation> {
        vec![target.observation.clone()]
    }
    fn snapshot(&self, target: &mut Target) -> Result<Snapshot, Box<dyn Error>> {
        target.capture()
    }
}

fn merge(evidence: &mut Evidence, observation: &Observation) {
    evidence.pairs |= observation.registers[10];
    if let Some(failure) = &observation.failure
        && !evidence.failures.contains(failure)
    {
        evidence.failures.push(failure.clone());
    }
}

impl Evaluation for NestedWorkload {
    fn execution_disposition(&self, target: &Target) -> ExecutionDisposition {
        target.observation.disposition()
    }
    fn objective_reached(&self, _: &(), target: &Target) -> Result<bool, Box<dyn Error>> {
        Ok(target.observation.failure.is_some())
    }
    fn current_key(&self, target: &Target) -> Result<Key, Box<dyn Error>> {
        let registers = target.observation.registers;
        Ok(Key([registers[6], registers[7], registers[9]]))
    }
    fn complete_candidate_key(&self, key: Key, _: &Snapshot) -> Result<Key, Box<dyn Error>> {
        Ok(key)
    }
    fn merge_milestones(&self, into: &mut u64, from: u64) {
        *into |= from;
    }
    fn aggregate_milestones(evidence: &Evidence) -> u64 {
        evidence.pairs
    }
    fn aggregate_progress(evidence: &Evidence) -> u32 {
        evidence.pairs.count_ones()
    }
    fn merge_origin_evidence(&self, evidence: &mut Evidence, source: &Report) {
        *evidence = source.evidence.clone();
    }
    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut Evidence,
        target: &Target,
    ) -> Result<(), Box<dyn Error>> {
        merge(evidence, &target.observation);
        Ok(())
    }
    fn merge_import_evidence(&self, evidence: &mut Evidence, milestones: u64, _: &Input<u64>) {
        evidence.pairs |= milestones;
    }
    fn merge_preparation_failure(&self, evidence: &mut Evidence, observations: &[Observation]) {
        for observation in observations {
            merge(evidence, observation);
        }
    }
    fn merge_action_evidence<F>(
        &self,
        evidence: &mut Evidence,
        action: &CampaignActionResult<Self>,
        _: u64,
        _: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<Input<u64>, Box<dyn Error>>,
    {
        for observation in &action.observations {
            merge(evidence, observation);
        }
        Ok(())
    }
    fn source_entries<'a>(&self, source: &'a Report) -> &'a [ArchiveEntryReport<u64, Key, u64>] {
        &source.entries
    }
    fn resume_input(&self, _: &Report) -> Result<Input<u64>, Box<dyn Error>> {
        Err("use the recorded nested campaign stream for replay".into())
    }
}

impl Reporting for NestedWorkload {
    fn stream_format(&self) -> &'static str {
        "nested-consonance-campaign-v2"
    }
    fn checkpoint_format(&self) -> &'static str {
        "nested-consonance-checkpoint-v2"
    }
    fn workload_identity_sha256(&self) -> String {
        format!("{:x}", Sha256::digest(self.identity.as_bytes()))
    }
    fn action_cost_unit(&self) -> &'static str {
        "sdk_operations"
    }
    fn execution_work_unit(&self) -> &'static str {
        "sdk_operations"
    }
    fn result_sha256(&self, result: &CampaignJobResult<Self>) -> Result<String, Box<dyn Error>> {
        let mut canonical = result.clone();
        for action in &mut canonical.actions {
            if let Some(candidate) = &mut action.candidate {
                let snapshot = &mut candidate.snapshot;
                let sidecar = vmm_core::portable_snapshot::logical_x86_sparse_sidecar(
                    &snapshot.state.sidecar(),
                )?;
                snapshot.state = SparseSnapshot::from_parts(
                    snapshot.state.base(),
                    snapshot.state.image_identity(),
                    snapshot.state.pages().to_vec(),
                    &sidecar,
                    None,
                )?;
            }
        }
        campaign::postcard_result_sha256(&canonical)
    }
    fn evidence_checkpoint(evidence: &Evidence) -> Result<Vec<u8>, Box<dyn Error>> {
        Ok(serde_json::to_vec(evidence)?)
    }
    fn evidence_from_checkpoint(bytes: &[u8]) -> Result<Evidence, Box<dyn Error>> {
        Ok(serde_json::from_slice(bytes)?)
    }
    fn archive_report(&self, evidence: &Evidence, state: ArchiveReportState<Self>) -> Report {
        Report {
            executions: state.executions,
            entries: state.entries,
            evidence: evidence.clone(),
        }
    }
}

pub fn run(
    image: &str,
    kernel: &[u8],
    base: &[u8],
    options: &Options,
    replay: Option<&Path>,
    repeat: u32,
) -> Result<(), Box<dyn Error>> {
    if options.executions == 0 || options.ram_mib == 0 || repeat == 0 {
        return Err("search budgets, RAM and repeat must be positive".into());
    }
    if options.output.exists() && fs::read_dir(&options.output)?.next().is_some() {
        return Err("search output directory must be empty".into());
    }
    let staging = tempfile::tempdir()?;
    let image = oci_support::image::stage(image, staging.path())?;
    let request = oci_support::bundle::LaunchRequest::new(vec![
        "/app/nested-driver".into(),
        "--sdk".into(),
        "--search".into(),
    ])
    .with_kvm();
    let initramfs = oci_support::bundle::prepare(&image, &request)?.initramfs(base);
    let workload = NestedWorkload::new(kernel, &initramfs, options);
    fs::create_dir_all(&options.output)?;
    let (report, _) = match replay {
        Some(path) => {
            let bytes = fs::read(path)?;
            let mut final_report = None;
            for run in 0..repeat {
                let replayed =
                    campaign::replay_campaign_checkpointed(&workload, &bytes, None, None)?;
                fs::write(
                    options.output.join(format!("replay-{run}.json")),
                    serde_json::to_vec_pretty(&replayed.0)?,
                )?;
                final_report = Some(replayed);
            }
            final_report.ok_or("no replay ran")?
        }
        None => {
            let config = CampaignConfig {
                campaign_seed: options.seed,
                workers: 1,
                execution_budget: options.executions,
                host: "linux-x86-nested-vmx".into(),
                wall_budget: options
                    .wall_minutes
                    .map(|minutes| Duration::from_secs(minutes.saturating_mul(60))),
                stop_rollout_on_objective: true,
                stop_campaign_on_objective: true,
                archive_entry_limit: 4096,
                window: campaign::default_window(1),
                memory_budget_mib: Some(1024),
                materialize_final_artifacts: false,
                run: (),
                suffix: SuffixShape::default(),
                mixture: DrawMixture::default(),
                retention: RetentionPolicy::Unprobed,
                objective_witness_path: Some(options.output.join("first-failure-input.json")),
            };
            let mut stream = BufWriter::new(fs::File::create(options.output.join("stream.jsonl"))?);
            let mut progress = fs::File::create(options.output.join("progress.jsonl"))?;
            campaign::run_campaign_checkpointed(
                &workload,
                &config,
                &CampaignOrigin::Genesis,
                &mut stream,
                Some(&mut progress),
            )?
        }
    };
    fs::write(
        options.output.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    for (index, failure) in report.archive.evidence.failures.iter().enumerate() {
        fs::write(
            options.output.join(format!("failure-{index}.json")),
            serde_json::to_vec_pretty(failure)?,
        )?;
    }
    println!(
        "nested_search executions={} operations={} pairs={}/36 mask={:x} failures={}",
        report.executions_completed,
        report.execution_work,
        report.archive.evidence.pairs.count_ones(),
        report.archive.evidence.pairs,
        report.archive.evidence.failures.len()
    );
    if !report.archive.evidence.failures.is_empty() {
        return Err("nested search found a failure; replay its stream.jsonl".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use searcher::search::rollout::{Outcome, Rollout, execute_suffix};

    struct FailedParent(Observation);

    impl Rollout<NestedWorkload> for FailedParent {
        fn apply(&mut self, _: &u64, _: &mut u64) -> Result<(), Box<dyn Error>> {
            panic!("a failed parent must not execute suffix actions")
        }
        fn observations(&self) -> Vec<Observation> {
            vec![self.0.clone()]
        }
        fn outcome(&self) -> Result<Outcome, Box<dyn Error>> {
            Ok(Outcome {
                objective_reached: self.0.failure.is_some(),
                disposition: self.0.disposition(),
            })
        }
        fn snapshot(&mut self) -> Result<Snapshot, Box<dyn Error>> {
            panic!("a failed parent must not publish a candidate")
        }
        fn key(&self) -> Result<Key, Box<dyn Error>> {
            panic!("a failed parent must not publish a key")
        }
        fn probe(&mut self, _: &Snapshot) -> Result<bool, Box<dyn Error>> {
            panic!("a failed parent must not be probed")
        }
    }

    fn failed_observation() -> Observation {
        let mut registers = [0; 11];
        registers[10] = (1 << 35) | 1;
        Observation {
            registers,
            failure: Some(Failure {
                layer: "outer-VMM".into(),
                assertion: None,
                detail: "outer restore changed the inner counts or published state".into(),
                actions: vec![42, 19],
            }),
        }
    }

    #[test]
    fn parent_restore_failure_reaches_serialized_report_and_checkpoint() {
        let observation = failed_observation();
        let result = execute_suffix(
            &mut FailedParent(observation.clone()),
            0,
            &[7],
            RetentionPolicy::Unprobed,
            true,
        )
        .unwrap();
        assert!(result.actions.is_empty());
        assert_eq!(result.preparation_failure, Some(vec![observation.clone()]));
        let workload = NestedWorkload::new(
            &[],
            &[],
            &Options {
                seed: 42,
                executions: 1,
                ram_mib: 256,
                wall_minutes: None,
                output: PathBuf::new(),
            },
        );
        let mut evidence = Evidence::default();
        workload
            .merge_preparation_failure(&mut evidence, result.preparation_failure.as_ref().unwrap());
        let checkpoint = NestedWorkload::evidence_checkpoint(&evidence).unwrap();
        let restored = NestedWorkload::evidence_from_checkpoint(&checkpoint).unwrap();
        let report = workload.archive_report(
            &restored,
            ArchiveReportState {
                seed: 42,
                executions: 1,
                entries: Vec::new(),
                progress_curve: Vec::new(),
                retained: 1,
                rejected: 0,
                terminal_endpoints: 0,
                terminal_objectives: 0,
                execution_failures: 1,
                selector: Default::default(),
            },
        );
        let decoded: Report =
            serde_json::from_slice(&serde_json::to_vec(&report).unwrap()).unwrap();
        assert_eq!(decoded.evidence.pairs, observation.registers[10]);
        assert_eq!(
            decoded.evidence.failures,
            vec![observation.failure.unwrap()]
        );
    }

    #[test]
    fn reset_failure_survives_the_following_parent_restore() {
        let original = failed_observation();
        let mut observation = original.clone();
        assert!(!observation.prepare_restore(&Observation::default()));
        assert_eq!(observation, original);
        assert_eq!(observation.disposition(), ExecutionDisposition::Failed);
        observation.failure = None;
        assert!(observation.prepare_restore(&Observation::default()));
        assert_eq!(observation.disposition(), ExecutionDisposition::Runnable);
    }
}
