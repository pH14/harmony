// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::{Condvar, Mutex};

static CHECKPOINT_DIRECTORY_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct RunWatch {
    heap: usize,
    reorder: bool,
    fail_task: Option<u64>,
    later_leaf: (Mutex<bool>, Condvar),
    physical_edges: Mutex<Vec<ExpansionEdgeId>>,
}

impl RunWatch {
    fn new(heap: usize, reorder: bool, fail_task: Option<u64>) -> Self {
        Self {
            heap,
            reorder,
            fail_task,
            later_leaf: (Mutex::new(false), Condvar::new()),
            physical_edges: Mutex::new(Vec::new()),
        }
    }

    fn release_first(&self) {
        *self.later_leaf.0.lock().unwrap() = true;
        self.later_leaf.1.notify_all();
    }
}

impl ExpansionWatch<TestWorkload> for RunWatch {
    type Note = Vec<u8>;

    fn note_memory_charge(&self, note: &Vec<u8>) -> usize {
        size_of::<Vec<u8>>() + crate::search::heap::block(note.capacity())
    }

    fn run_edge(
        &self,
        id: ExpansionEdgeId,
        _job: &JobSpec<TestWorkload>,
        execute: impl FnOnce() -> Result<JobOutcome<TestWorkload>, Box<dyn Error>>,
    ) -> WatchedJob<TestWorkload, Vec<u8>> {
        if self.fail_task == Some(id.task) {
            self.physical_edges.lock().unwrap().push(id);
            self.release_first();
            return Err("injected later physical worker error".into());
        }
        if self.reorder && id.task == 0 && id.ordinal == 1 {
            let (ready, timeout) = self
                .later_leaf
                .1
                .wait_timeout_while(
                    self.later_leaf.0.lock().unwrap(),
                    Duration::from_secs(10),
                    |ready| !*ready,
                )
                .unwrap();
            if timeout.timed_out() || !*ready {
                return Err("later physical task did not reach a leaf".into());
            }
        }
        let result = execute()?;
        self.physical_edges.lock().unwrap().push(id);
        if id.task > 0 && id.ordinal >= 15 {
            self.release_first();
        }
        Ok((result, vec![7; self.heap]))
    }
}

fn run_config(workers: u32) -> CampaignConfig<TestWorkload> {
    CampaignConfig {
        campaign_seed: 19,
        workers,
        execution_budget: 56,
        host: "expansion-worker-runtime-fixture".into(),
        wall_budget: None,
        stop_rollout_on_objective: true,
        stop_campaign_on_objective: false,
        archive_entry_limit: 512,
        window: 3,
        memory_budget_mib: None,
        materialize_final_artifacts: true,
        run: (),
        mixture: DrawMixture::AlphabetOnly,
        retention: RetentionPolicy::Unprobed,
        objective_witness_path: None,
    }
}

type RunCase = (
    Result<ExpansionOutcome<TestWorkload>, Box<dyn Error>>,
    Vec<u8>,
    RunWatch,
);

fn run_case(workers: u32, heap: usize, reorder: bool, fail_task: Option<u64>) -> RunCase {
    let workload = TestWorkload::new(Mode::Normal);
    let config = run_config(workers);
    let watch = RunWatch::new(heap, reorder, fail_task);
    let mut stream = Vec::new();
    let result = run_expansion_watched(
        &workload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
        CampaignExecutionOptions::default(),
        &watch,
    );
    (result, stream, watch)
}

fn records(stream: &[u8]) -> Vec<serde_json::Value> {
    stream
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}

fn encoded(records: &[serde_json::Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    bytes
}

#[test]
fn expansion_runtime_stream_report_and_checkpoint_ignore_worker_count_and_physical_order() {
    let (single, one_stream, _) = run_case(1, 0, false, None);
    let (parallel, three_stream, watch) = run_case(3, 0, true, None);
    let (one, one_checkpoint) = single.unwrap();
    let (three, three_checkpoint) = parallel.unwrap();
    assert_eq!(one_stream, three_stream);
    assert_eq!(
        serde_json::to_vec(&one).unwrap(),
        serde_json::to_vec(&three).unwrap()
    );
    assert_eq!(
        one_checkpoint.to_bytes().unwrap(),
        three_checkpoint.to_bytes().unwrap()
    );
    assert!(one.counters.tasks_admitted > 3);
    assert_eq!(one.counters.tasks_reserved, one.counters.tasks_admitted);
    assert_eq!(one.counters.budget_consumed, 56);
    assert!(!one.archive.entries.is_empty());
    let physical = watch.physical_edges.lock().unwrap();
    let first = physical.iter().position(|id| id.task == 0).unwrap();
    assert!(
        physical[..first]
            .iter()
            .any(|id| id.task > 0 && id.ordinal >= 15)
    );
    assert_eq!(
        three
            .telemetry
            .workers
            .iter()
            .map(|worker| worker.jobs)
            .sum::<u64>(),
        three.counters.tasks_admitted
    );
    assert!(!three_checkpoint.entries.is_empty());
}

#[test]
fn expansion_runtime_whole_coordinator_cpu_encloses_dispatch_and_stream_finish() {
    let supported = crate::search::telemetry::thread_schedstat().is_some();
    let (result, _, _) = run_case(1, 0, false, None);
    let (report, _) = result.unwrap();
    let telemetry = report.telemetry;
    if supported {
        let whole = telemetry.coordinator_host.unwrap();
        let dispatch = telemetry.dispatch_phase.unwrap().host.unwrap();
        let finish = if profile_clock_enabled() {
            telemetry.coordinator["stream_finish_host"]["coordinator_cpu_ns"]
                .as_u64()
                .unwrap()
        } else {
            0
        };
        assert!(
            whole.coordinator_cpu_ns >= dispatch.coordinator_cpu_ns.checked_add(finish).unwrap()
        );
        assert!(whole.coordinator_run_wait_ns >= dispatch.coordinator_run_wait_ns);
        assert_eq!(
            whole.idle_admission_order_ns,
            telemetry.admission.idle_admission_order_ns
        );
        assert_eq!(whole.idle_no_job_ns, telemetry.admission.idle_no_job_ns);
    } else {
        assert!(telemetry.coordinator_host.is_none());
    }
}

#[test]
fn expansion_runtime_codec_records_every_node_and_keeps_observation_commitments_on_roundtrip() {
    let ((report, _), stream) = {
        let (result, stream, _) = run_case(1, 0, false, None);
        (result.unwrap(), stream)
    };
    let rows = records(&stream);
    let mut task = 0;
    let mut attempts = 0;
    let mut seeds = BTreeSet::new();
    for value in &rows[1..] {
        let decoded: ExpansionStreamRecord<TestWorkload> =
            serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&decoded).unwrap(), *value);
        match decoded {
            ExpansionStreamRecord::Task(record) => {
                assert_eq!(record.reservation.task, task);
                task += 1;
                assert!(seeds.insert(record.reservation.task_seed));
                assert!(record.reserved_through >= task);
                assert_eq!(
                    record.reservation.replay.len(),
                    record.reservation.replay_certificates.len()
                );
                assert!(!record.decisions.is_empty());
                assert!(!record.frontier.is_empty());
                assert_eq!(record.nodes[0].ordinal, 0);
                assert!(record.nodes[0].evidence.is_none());
                assert_eq!(
                    record.nodes.len() - 1,
                    record
                        .accounting
                        .as_ref()
                        .unwrap()
                        .task_unique_edge_attempts
                );
                attempts += record.nodes.len() - 1;
                for node in &record.nodes[1..] {
                    let evidence = node.evidence.as_ref().unwrap();
                    assert!(node.parent_ordinal.is_some());
                    assert_eq!(node.suffix.len(), usize::from(node.depth));
                    assert_eq!(node.suffix.last(), Some(&evidence.action));
                    assert_eq!(node.edge_cost, u64::from(evidence.action));
                    assert_eq!(
                        node.path_cost,
                        node.suffix
                            .iter()
                            .map(|action| u64::from(*action))
                            .sum::<u64>()
                    );
                    assert_eq!(
                        node.cost_in_group,
                        Some(record.reservation.root_cost_in_group + node.path_cost)
                    );
                    assert_eq!(node.key, evidence.candidate_key);
                    assert_eq!(node.milestones, Some(evidence.milestones));
                    assert_eq!(evidence.replay_certificate, Some([node.key.unwrap().0; 32]));
                    assert_eq!(evidence.observations.count(), 1);
                    assert_eq!(evidence.result_sha256.len(), 64);
                }
            }
            ExpansionStreamRecord::End {
                counters,
                stop_reason,
            } => {
                assert_eq!(counters, report.counters);
                assert_eq!(stop_reason, report.stop_reason);
            }
        }
    }
    assert_eq!(task, report.counters.tasks_admitted);
    assert_eq!(attempts as u64, report.counters.edge_attempts);
    let original = rows.iter().find(|value| value["record"] == "task").unwrap();
    let mut planted = original.clone();
    planted["reservation"]["task_seed"] =
        (planted["reservation"]["task_seed"].as_u64().unwrap() ^ 1).into();
    planted["nodes"][1]["path_cost"] = 999.into();
    planted["nodes"][1]["evidence"]["observations"]["count"] = 211.into();
    planted["nodes"][1]["evidence"]["replay_certificate"][0] = 212.into();
    let decoded: ExpansionStreamRecord<TestWorkload> =
        serde_json::from_value(planted.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), planted);
    assert_ne!(&planted, original);
}

#[test]
fn expansion_runtime_observer_heap_has_no_semantic_stream_report_or_checkpoint_effect() {
    let (small, small_stream, _) = run_case(3, 0, false, None);
    let (large, large_stream, _) = run_case(3, 16 * 1024, false, None);
    let (small, small_checkpoint) = small.unwrap();
    let (large, large_checkpoint) = large.unwrap();
    assert_eq!(small_stream, large_stream);
    assert_eq!(
        serde_json::to_value(small).unwrap(),
        serde_json::to_value(large).unwrap()
    );
    assert_eq!(
        small_checkpoint.to_bytes().unwrap(),
        large_checkpoint.to_bytes().unwrap()
    );
}

#[test]
fn expansion_runtime_later_physical_worker_error_flushes_only_earlier_successful_task_prefix() {
    let (result, stream, watch) = run_case(3, 1024, true, Some(1));
    let error = result.err().unwrap();
    assert!(
        error
            .to_string()
            .contains("injected later physical worker error")
    );
    let rows = records(&stream);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["format"], EXPANSION_STREAM_FORMAT);
    assert_eq!(rows[1]["record"], "task");
    assert_eq!(rows[1]["reservation"]["task"], 0);
    assert!(rows.iter().all(|value| value["record"] != "end"));
    let physical = watch.physical_edges.lock().unwrap();
    let error = physical.iter().position(|id| id.task == 1).unwrap();
    let first = physical.iter().position(|id| id.task == 0).unwrap();
    assert!(error < first);
}

#[test]
fn expansion_runtime_full_replay_matches_checkpoint_and_rejects_each_forged_tree_field() {
    let (result, stream, _) = run_case(3, 4096, true, None);
    let (report, checkpoint) = result.unwrap();
    let workload = TestWorkload::new(Mode::Normal);
    let mut input = stream.as_slice();
    let (replayed, replayed_checkpoint) = replay_expansion(
        &workload,
        &run_config(1),
        &CampaignOrigin::Genesis,
        &mut input,
        &report,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_vec(&replayed).unwrap(),
        serde_json::to_vec(&report).unwrap()
    );
    assert_eq!(
        checkpoint.to_bytes().unwrap(),
        replayed_checkpoint.to_bytes().unwrap()
    );
    let original = records(&stream);
    for field in ["observation", "seed", "frontier", "decision", "certificate"] {
        let mut rows = original.clone();
        let record = &mut rows[1];
        match field {
            "observation" => {
                record["nodes"][1]["evidence"]["observations"]["sha256"] = "0".repeat(64).into()
            }
            "seed" => {
                record["reservation"]["task_seed"] =
                    (record["reservation"]["task_seed"].as_u64().unwrap() ^ 1).into()
            }
            "frontier" => record["frontier"] = serde_json::json!([0]),
            "decision" => record["decisions"][0]["sequence"] = 999.into(),
            "certificate" => record["nodes"][1]["evidence"]["replay_certificate"][0] = 212.into(),
            _ => unreachable!(),
        }
        let planted = encoded(&rows);
        let mut forged_expected: ExpansionRunReport<u8, Report> =
            serde_json::from_value(serde_json::to_value(&report).unwrap()).unwrap();
        forged_expected.stream_sha256 = format!("{:x}", Sha256::digest(&planted));
        assert!(
            replay_expansion(
                &workload,
                &run_config(1),
                &CampaignOrigin::Genesis,
                &mut planted.as_slice(),
                &forged_expected
            )
            .is_err(),
            "accepted planted {field}"
        );
    }
}

#[test]
fn expansion_replay_rejects_self_consistent_premature_end_and_unavailable_stop_reasons() {
    let workload = TestWorkload::new(Mode::Normal);
    let mut selected = None;
    for seed in 0..32 {
        let mut config = run_config(1);
        config.campaign_seed = seed;
        config.execution_budget = 14;
        config.window = 1;
        let mut stream = Vec::new();
        let (report, _) = run_expansion_watched(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
            CampaignExecutionOptions::default(),
            &(),
        )
        .unwrap();
        if report.counters.tasks_admitted == 1 && report.counters.budget_consumed == 14 {
            selected = Some((config, report, stream));
            break;
        }
    }
    let (mut config, report, stream) = selected.expect("a full fourteen-edge fixture task");
    assert_eq!(report.stop_reason, ExpansionStopReason::Budget);
    let rows = records(&stream);
    assert_eq!(rows.len(), 3);
    let original: ExpansionTaskRecord<TestWorkload> =
        match serde_json::from_value::<ExpansionStreamRecord<TestWorkload>>(rows[1].clone())
            .unwrap()
        {
            ExpansionStreamRecord::Task(record) => record,
            ExpansionStreamRecord::End { .. } => panic!("expected task"),
        };
    assert_eq!(original.reservation.edge_budget.get(), 14);
    assert_eq!(original.accounting.unwrap().task_unique_edge_attempts, 14);
    let mut forged: ExpansionRunReport<u8, Report> =
        serde_json::from_value(serde_json::to_value(&report).unwrap()).unwrap();
    let mut premature = rows.clone();
    config.execution_budget = 56;
    forged.header.execution_budget = 56;
    premature[0]["execution_budget"] = 56.into();
    let bytes = encoded(&premature);
    forged.stream_sha256 = format!("{:x}", Sha256::digest(&bytes));
    let error = replay_expansion(
        &workload,
        &config,
        &CampaignOrigin::Genesis,
        &mut bytes.as_slice(),
        &forged,
    )
    .err()
    .unwrap();
    assert!(
        error
            .to_string()
            .contains("no sufficient stopping condition")
    );
    config.execution_budget = 14;
    for reason in [
        ExpansionStopReason::WorkBudget,
        ExpansionStopReason::Objective,
        ExpansionStopReason::WallBudget,
    ] {
        let mut rows = rows.clone();
        let last = rows.last_mut().unwrap();
        last["stop_reason"] = serde_json::to_value(reason).unwrap();
        let bytes = encoded(&rows);
        let mut forged: ExpansionRunReport<u8, Report> =
            serde_json::from_value(serde_json::to_value(&report).unwrap()).unwrap();
        forged.stop_reason = reason;
        forged.stream_sha256 = format!("{:x}", Sha256::digest(&bytes));
        let error = replay_expansion(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut bytes.as_slice(),
            &forged,
        )
        .err()
        .unwrap();
        assert!(
            error
                .to_string()
                .contains("no sufficient stopping condition"),
            "{reason:?}: {error}"
        );
    }
}

#[test]
fn expansion_runtime_resumes_pending_pool_tasks_and_replays_the_resumed_stream() {
    let directory = std::env::temp_dir().join(format!(
        "harmony-expansion-pool-resume-{}-{}",
        std::process::id(),
        CHECKPOINT_DIRECTORY_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let workload = TestWorkload::new(Mode::Normal);
    let config = run_config(3);
    let plan = CheckpointPlan {
        directory: directory.clone(),
        every: std::num::NonZeroU64::new(28),
        on_marks: false,
        on_top_progress: false,
        checkpoint_owner: None,
        max_image_bytes: 64 << 20,
        before_write: None,
    };
    let mut original_stream = Vec::new();
    let (original, original_snapshot) = run_expansion_watched(
        &workload,
        &config,
        &CampaignOrigin::Genesis,
        &mut original_stream,
        None,
        CampaignExecutionOptions {
            checkpoints: Some(plan),
            ..CampaignExecutionOptions::default()
        },
        &RunWatch::new(0, false, None),
    )
    .unwrap();
    let mut intervals =
        std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "ckpt")
            })
            .filter_map(|path| {
                let header = crate::search::checkpoint::read_header(&path).unwrap();
                (header.reason == "interval" && (header.admitted as u64) < header.reserved)
                    .then_some((header.executions, path, header))
            })
            .collect::<Vec<_>>();
    intervals.sort_by_key(|(executions, _, _)| *executions);
    let (_, path, header) = intervals
        .into_iter()
        .next()
        .expect("interval with pending tasks");
    assert!(header.executions >= 28);
    assert!(header.executions < config.execution_budget);
    let origin = CampaignOrigin::SearchCheckpoint {
        path,
        expected_sha256: None,
    };
    let mut resumed_stream = Vec::new();
    let (resumed, resumed_snapshot) = run_expansion_watched(
        &workload,
        &config,
        &origin,
        &mut resumed_stream,
        None,
        CampaignExecutionOptions::default(),
        &RunWatch::new(0, false, None),
    )
    .unwrap();
    assert_eq!(
        resumed.initial_counters.tasks_admitted,
        header.admitted as u64
    );
    assert_eq!(resumed.initial_counters.tasks_reserved, header.reserved);
    assert!(resumed.initial_counters.tasks_admitted < resumed.initial_counters.tasks_reserved);
    assert_eq!(resumed.counters, original.counters);
    assert_eq!(
        serde_json::to_vec(&resumed.archive).unwrap(),
        serde_json::to_vec(&original.archive).unwrap()
    );
    assert_eq!(resumed.objective_witness, original.objective_witness);
    assert_eq!(
        resumed_snapshot.to_bytes().unwrap(),
        original_snapshot.to_bytes().unwrap()
    );
    assert_eq!(
        resumed
            .telemetry
            .workers
            .iter()
            .map(|worker| worker.jobs)
            .sum::<u64>(),
        resumed.counters.tasks_admitted - resumed.initial_counters.tasks_admitted
    );
    let (replayed, replayed_snapshot) = replay_expansion(
        &workload,
        &config,
        &origin,
        &mut resumed_stream.as_slice(),
        &resumed,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_vec(&replayed).unwrap(),
        serde_json::to_vec(&resumed).unwrap()
    );
    assert_eq!(
        replayed_snapshot.to_bytes().unwrap(),
        resumed_snapshot.to_bytes().unwrap()
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn expansion_runtime_checkpoints_each_crossed_interval_bucket_at_task_boundaries() {
    let directory = std::env::temp_dir().join(format!(
        "harmony-expansion-crossed-interval-{}-{}",
        std::process::id(),
        CHECKPOINT_DIRECTORY_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let workload = TestWorkload::new(Mode::Normal);
    let config = run_config(3);
    let mut stream = Vec::new();
    let (report, _) = run_expansion_watched(
        &workload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
        CampaignExecutionOptions {
            checkpoints: Some(CheckpointPlan {
                directory: directory.clone(),
                every: std::num::NonZeroU64::new(13),
                on_marks: false,
                on_top_progress: false,
                checkpoint_owner: None,
                max_image_bytes: 64 << 20,
                before_write: None,
            }),
            ..CampaignExecutionOptions::default()
        },
        &RunWatch::new(0, false, None),
    )
    .unwrap();
    let mut previous = 0;
    let mut expected = Vec::new();
    for record in records(&stream) {
        if record["record"] != "task" {
            continue;
        }
        let decisions = record["decisions"].as_array().unwrap();
        let Some(last) = decisions.last() else {
            continue;
        };
        let sequence = last["sequence"].as_u64().unwrap();
        if sequence / 13 > previous / 13 {
            expected.push(sequence);
        }
        previous = sequence;
    }
    assert!(expected.len() >= 3);
    assert!(expected.iter().any(|sequence| !sequence.is_multiple_of(13)));
    let lines = std::fs::read(directory.join("checkpoints.jsonl")).unwrap();
    let rows = records(&lines);
    let actual = rows
        .iter()
        .filter(|row| row["reason"] == "interval")
        .map(|row| row["executions"].as_u64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    let final_rows = rows
        .iter()
        .filter(|row| row["reason"] == "final")
        .collect::<Vec<_>>();
    assert_eq!(final_rows.len(), 1);
    assert_eq!(final_rows[0]["executions"], report.counters.edge_attempts);
    let retained_intervals = std::fs::read_dir(&directory)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .ends_with("-interval.ckpt")
                .then_some(path)
        })
        .count();
    assert_eq!(retained_intervals, actual.len().min(2));
    std::fs::remove_dir_all(directory).unwrap();
}

struct ObservationMutationWatch {
    after_admission: bool,
    changed: AtomicUsize,
}

impl ExpansionWatch<TestWorkload> for ObservationMutationWatch {
    type Note = ();

    fn note_memory_charge(&self, _: &()) -> usize {
        0
    }

    fn run_edge(
        &self,
        _: ExpansionEdgeId,
        _: &JobSpec<TestWorkload>,
        execute: impl FnOnce() -> Result<JobOutcome<TestWorkload>, Box<dyn Error>>,
    ) -> WatchedJob<TestWorkload, ()> {
        let mut outcome = execute()?;
        if !self.after_admission {
            let before = outcome.result_sha256.clone();
            for action in &mut outcome.result.actions {
                action.observations[0] ^= 128;
                self.changed.fetch_add(1, Ordering::Relaxed);
            }
            assert_eq!(before, outcome.result_sha256);
        }
        Ok((outcome, ()))
    }

    fn admitted(
        &self,
        admission: &mut ExpansionAdmission<TestWorkload, ()>,
        _: &ExpansionCoordinator<'_, TestWorkload>,
    ) {
        if self.after_admission {
            for node in admission
                .outcome
                .trace
                .as_mut()
                .unwrap()
                .nodes
                .iter_mut()
                .skip(1)
            {
                let evidence = &mut node.evidence.as_mut().unwrap().evidence;
                let before = evidence.result_sha256.clone();
                evidence.observations[0] ^= 128;
                assert_eq!(before, evidence.result_sha256);
                self.changed.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[test]
fn expansion_commitments_reject_post_execution_and_post_admission_mutations_with_stale_result_sha()
{
    let workload = TestWorkload::new(Mode::Normal);
    let mut config = run_config(1);
    config.execution_budget = 14;
    config.window = 1;
    let mut reference_stream = Vec::new();
    let (reference, reference_snapshot) = run_expansion_watched(
        &workload,
        &config,
        &CampaignOrigin::Genesis,
        &mut reference_stream,
        None,
        CampaignExecutionOptions::default(),
        &(),
    )
    .unwrap();
    for after_admission in [false, true] {
        let watch = ObservationMutationWatch {
            after_admission,
            changed: AtomicUsize::new(0),
        };
        let mut stream = Vec::new();
        let (report, snapshot) = run_expansion_watched(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            None,
            CampaignExecutionOptions::default(),
            &watch,
        )
        .unwrap();
        assert!(watch.changed.load(Ordering::Relaxed) > 0);
        assert_eq!(
            serde_json::to_vec(&report.archive).unwrap(),
            serde_json::to_vec(&reference.archive).unwrap()
        );
        assert_eq!(report.counters, reference.counters);
        assert_eq!(
            snapshot.to_bytes().unwrap(),
            reference_snapshot.to_bytes().unwrap()
        );
        let original = records(&reference_stream);
        let changed = records(&stream);
        assert_eq!(
            original[1]["nodes"][1]["evidence"]["result_sha256"],
            changed[1]["nodes"][1]["evidence"]["result_sha256"]
        );
        assert_ne!(
            original[1]["nodes"][1]["evidence"]["observations"],
            changed[1]["nodes"][1]["evidence"]["observations"]
        );
        let error = replay_expansion(
            &workload,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream.as_slice(),
            &report,
        )
        .unwrap_err();
        assert!(error.to_string().contains("replay differs in its tree"));
    }
}

#[test]
fn expansion_commitments_preserve_root_and_edge_preparation_failure_evidence() {
    let workload = TestWorkload::new(Mode::RootPreparation);
    let root_spec = spec(vec![2]);
    let failed = run_expansion_task(
        &workload,
        &(),
        &mut workload.new_target().unwrap(),
        &root_spec,
        &(),
    )
    .unwrap();
    let mut admission = ExpansionAdmission {
        spec: root_spec,
        outcome: failed,
        decisions: Vec::new(),
        useful: false,
    };
    let raw = ExpansionTaskRecord::take(&mut admission, 4);
    let wire = serde_json::to_vec(&raw).unwrap();
    let decoded: ExpansionTaskRecord<TestWorkload> = serde_json::from_slice(&wire).unwrap();
    assert_eq!(decoded.preparation_failure.as_ref().unwrap().count(), 1);
    assert_eq!(serde_json::to_vec(&decoded).unwrap(), wire);
    let mut changed = serde_json::to_value(&decoded).unwrap();
    changed["preparation_failure"]["sha256"] = "0".repeat(64).into();
    let changed: ExpansionTaskRecord<TestWorkload> = serde_json::from_value(changed).unwrap();
    assert_ne!(
        serde_json::to_vec(&raw).unwrap(),
        serde_json::to_vec(&changed).unwrap()
    );

    let workload = TestWorkload::new(Mode::EdgePreparation);
    let mut config = run_config(1);
    config.execution_budget = 4;
    config.window = 1;
    let mut stream = Vec::new();
    let (report, _) = run_expansion_watched(
        &workload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
        CampaignExecutionOptions::default(),
        &(),
    )
    .unwrap();
    replay_expansion(
        &workload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream.as_slice(),
        &report,
    )
    .unwrap();
    for field in ["digest", "count", "presence"] {
        let mut rows = records(&stream);
        let evidence = &mut rows[1]["nodes"][1]["evidence"];
        assert_eq!(evidence["preparation_failure"]["count"], 1);
        match field {
            "digest" => evidence["preparation_failure"]["sha256"] = "0".repeat(64).into(),
            "count" => evidence["preparation_failure"]["count"] = 0.into(),
            "presence" => evidence["preparation_failure"] = serde_json::Value::Null,
            _ => unreachable!(),
        }
        let planted = encoded(&rows);
        let mut expected: ExpansionRunReport<u8, Report> =
            serde_json::from_value(serde_json::to_value(&report).unwrap()).unwrap();
        expected.stream_sha256 = format!("{:x}", Sha256::digest(&planted));
        assert!(
            replay_expansion(
                &workload,
                &config,
                &CampaignOrigin::Genesis,
                &mut planted.as_slice(),
                &expected
            )
            .is_err()
        );
    }
}

#[test]
fn expansion_stream_handoff_releases_origin_before_blocked_outcome_cleanup() {
    let workload = TestWorkload::new(Mode::Normal);
    let mut config = run_config(1);
    config.window = 1;
    config.execution_budget = 14;
    let watch = RunWatch::new(1024, false, None);
    let mut campaign = ExpansionCoordinator::<TestWorkload, Vec<u8>>::start(
        &workload,
        &config,
        &CampaignOrigin::Genesis,
    )
    .unwrap();
    let spec = campaign.reserve().unwrap().unwrap();
    let origin = Arc::downgrade(&spec.snapshot);
    let outcome =
        run_expansion_task(&workload, &(), &mut Target::default(), &spec, &watch).unwrap();
    drop(spec);
    campaign.complete(outcome).unwrap();
    let mut admission = campaign.admit_next().unwrap().unwrap();
    watch.admitted(&mut admission, &campaign);
    let record = ExpansionTaskRecord::take(&mut admission, campaign.counters().tasks_reserved);
    assert!(!record.nodes.is_empty());
    assert_eq!(origin.strong_count(), 2);
    campaign
        .archive()
        .validate_inflight_pins(
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
    let cleanup = campaign.stream_cleanup(admission);
    assert!(cleanup.2.trace.as_ref().unwrap().nodes.iter().any(|node| {
        node.evidence
            .as_ref()
            .is_some_and(|edge| edge.note.as_ref().is_some_and(|note| note.len() == 1024))
    }));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        held_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        drop(cleanup);
    });
    held_rx.recv().unwrap();
    let while_blocked = origin.strong_count();
    release_tx.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(while_blocked, 1);
    assert_eq!(origin.strong_count(), 1);
}

struct PressureWatch {
    inner: RunWatch,
    compactions: std::sync::atomic::AtomicU64,
    entry_drops: std::sync::atomic::AtomicU64,
}

impl ExpansionWatch<TestWorkload> for PressureWatch {
    type Note = Vec<u8>;

    fn note_memory_charge(&self, note: &Self::Note) -> usize {
        self.inner.note_memory_charge(note)
    }

    fn admitted(
        &self,
        _: &mut ExpansionAdmission<TestWorkload, Self::Note>,
        campaign: &ExpansionCoordinator<'_, TestWorkload, Self::Note>,
    ) {
        self.compactions
            .fetch_max(campaign.archive().history_compactions(), Ordering::Relaxed);
        self.entry_drops
            .fetch_max(campaign.archive().entry_drops(), Ordering::Relaxed);
    }

    fn run_edge(
        &self,
        id: ExpansionEdgeId,
        job: &JobSpec<TestWorkload>,
        execute: impl FnOnce() -> Result<JobOutcome<TestWorkload>, Box<dyn Error>>,
    ) -> WatchedJob<TestWorkload, Self::Note> {
        self.inner.run_edge(id, job, execute)
    }
}

#[test]
fn expansion_runtime_replay_matches_under_bounded_memory_and_history_pressure() {
    let workload = TestWorkload::new(Mode::Normal);
    let mut config = run_config(2);
    config.execution_budget = 280;
    config.archive_entry_limit = 2;
    let mib = 1_usize << 20;
    let reserve = workload
        .draw_state_memory_reserve_bytes(&())
        .checked_add(crate::search::duration::DurationPolicies::<Key>::memory_reserve_bytes())
        .unwrap();
    let total = reserve.checked_add(512 << 10).unwrap();
    config.memory_budget_mib = Some(total.checked_add(mib - 1).unwrap() / mib);
    let archive_allowance = config.memory_budget_mib.unwrap().checked_mul(mib).unwrap() - reserve;
    assert!((512 << 10..(512 << 10) + mib).contains(&archive_allowance));
    let watch = PressureWatch {
        inner: RunWatch::new(1024, false, None),
        compactions: std::sync::atomic::AtomicU64::new(0),
        entry_drops: std::sync::atomic::AtomicU64::new(0),
    };
    let mut stream = Vec::new();
    let (report, snapshot) = run_expansion_watched(
        &workload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream,
        None,
        CampaignExecutionOptions::default(),
        &watch,
    )
    .unwrap();
    assert!(report.counters.tasks_admitted > 16);
    assert!(watch.compactions.load(Ordering::Relaxed) > 0);
    assert!(watch.entry_drops.load(Ordering::Relaxed) > 0);
    let (replayed, replayed_snapshot) = replay_expansion(
        &workload,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream.as_slice(),
        &report,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_vec(&report).unwrap(),
        serde_json::to_vec(&replayed).unwrap()
    );
    assert_eq!(
        snapshot.to_bytes().unwrap(),
        replayed_snapshot.to_bytes().unwrap()
    );
}
