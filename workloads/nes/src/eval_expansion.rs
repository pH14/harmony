// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::search::campaign::{
    ExpansionCounters, ExpansionRunReport, ExpansionStopReason, ExpansionWatch, ExpansionWorkload,
    replay_expansion, run_expansion_watched,
};

impl<G: Workload> EvaluationReport<G> for ExpansionRunReport<G::Action, G::ArchiveReport>
where
    G::ArchiveReport: Serialize,
{
    fn supports_resumed_replay() -> bool {
        true
    }

    fn archive(&self) -> &G::ArchiveReport {
        &self.archive
    }
    fn objective_witness(&self) -> Option<&Input<G::Action>> {
        self.objective_witness.as_ref()
    }
    fn stream_sha256(&self) -> &str {
        &self.stream_sha256
    }
    fn telemetry(&self) -> &crate::search::telemetry::CampaignTelemetry {
        &self.telemetry
    }
    fn metrics(&self) -> Result<ReportMetrics> {
        expansion_metrics(
            &self.counters,
            &self.initial_counters,
            self.stop_reason,
            self.objective_witness.is_some(),
        )
    }
}

fn expansion_metrics(
    counters: &ExpansionCounters,
    initial: &ExpansionCounters,
    stop_reason: ExpansionStopReason,
    victory_observed: bool,
) -> Result<ReportMetrics> {
    let execution_work = counters
        .bootstrap_work
        .checked_add(counters.reconstruction_work)
        .and_then(|work| work.checked_add(counters.execution_work))
        .ok_or("expansion total frame work overflow")?;
    let delta = |current: u64, previous: u64| {
        current
            .checked_sub(previous)
            .ok_or("expansion session counter moved backwards")
    };
    let session_executions = delta(counters.executed_edges, initial.executed_edges)?;
    let session_edge_work = delta(counters.execution_work, initial.execution_work)?;
    let session_reconstruction = delta(counters.reconstruction_work, initial.reconstruction_work)?;
    let session_bootstrap = delta(counters.bootstrap_work, initial.bootstrap_work)?;
    let session_work = session_edge_work
        .checked_add(session_reconstruction)
        .and_then(|work| work.checked_add(session_bootstrap))
        .ok_or("expansion session frame work overflow")?;
    let session = json!({"executed_edges":session_executions,"edge_frames":session_edge_work,
        "reconstruction_frames":session_reconstruction,"bootstrap_frames":session_bootstrap,"frames_emulated":session_work,
        "tasks_reserved":delta(counters.tasks_reserved,initial.tasks_reserved)?,
        "tasks_admitted":delta(counters.tasks_admitted,initial.tasks_admitted)?,
        "edge_attempts":delta(counters.edge_attempts,initial.edge_attempts)?,
        "budget_consumed":delta(counters.budget_consumed,initial.budget_consumed)?,
        "preparation_failures":delta(counters.preparation_failures,initial.preparation_failures)?,
        "empty_tasks":delta(counters.empty_tasks,initial.empty_tasks)?});
    Ok(ReportMetrics {
        executions: counters.executed_edges,
        execution_work,
        rate_executions: session_executions,
        rate_work: session_work,
        executions_to_first_objective: counters.executions_to_first_objective,
        work_to_first_objective: counters.work_to_first_objective,
        victory_observed,
        stop_reason: match stop_reason {
            ExpansionStopReason::Budget => Some("execution_limit"),
            ExpansionStopReason::WorkBudget => Some("frame_limit"),
            ExpansionStopReason::Objective => None,
            ExpansionStopReason::WallBudget => Some("wall_limit"),
        },
        extra: json!({"engine":"bounded_expansion", "window_semantics":"fixed_outstanding_tasks",
            "tasks_reserved":counters.tasks_reserved,"tasks_admitted":counters.tasks_admitted,
            "edge_attempts":counters.edge_attempts,"budget_consumed":counters.budget_consumed,
            "preparation_failures":counters.preparation_failures,"empty_tasks":counters.empty_tasks,
            "bootstrap_frames":counters.bootstrap_work,"reconstruction_frames":counters.reconstruction_work,
            "edge_frames":counters.execution_work,
            "initial_counters":initial,"session_counter_deltas":session,
            "rate_basis":"session_counter_deltas_over_search_seconds", "rates_include_startup_and_finalization_wall":true}),
    })
}

#[allow(clippy::too_many_arguments)]
pub fn evaluate_expansion_watched<G: ExpansionWorkload + 'static, W: ExpansionWatch<G>>(
    game: &G,
    run: G::Run,
    request: &Request,
    execution_sha256: &str,
    out: &Path,
    started: Instant,
    watch: &W,
) -> Result<()>
where
    G::ArchiveReport: Serialize + serde::de::DeserializeOwned,
    G::Action: 'static,
    G::Milestones: 'static,
    G::Observations: 'static,
    G::Snapshot: crate::witness::EndpointIdentity + 'static,
    <G::Key as ArchiveKey>::Progress: Send + Sync + 'static,
    G::ExpansionQuality: 'static,
    G::ExpansionBucket: 'static,
    W::Note: 'static,
{
    let identity = json!({"engine":"bounded_expansion", "expansion_policy":game.expansion_policy(), "window_semantics":"fixed_outstanding_tasks", "absolute_task_window":request.window});
    evaluate_engine(
        game,
        run,
        request,
        execution_sha256,
        out,
        started,
        request.window,
        Some(identity),
        |game, config, origin, stream, progress, options| {
            run_expansion_watched(game, config, origin, stream, progress, options, watch)
        },
        |game, config, origin, mut bytes, expected| {
            replay_expansion(game, config, origin, &mut bytes, expected)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expansion_metrics_keep_executed_edges_attempts_tasks_and_reconstruction_distinct() {
        let counters = ExpansionCounters {
            tasks_reserved: 2,
            tasks_admitted: 2,
            budget_consumed: 8,
            edge_attempts: 7,
            executed_edges: 5,
            preparation_failures: 2,
            empty_tasks: 1,
            bootstrap_work: 3,
            reconstruction_work: 11,
            execution_work: 29,
            executions_to_first_objective: Some(4),
            work_to_first_objective: Some(40),
        };
        let metrics = expansion_metrics(
            &counters,
            &ExpansionCounters {
                bootstrap_work: 3,
                ..Default::default()
            },
            ExpansionStopReason::Budget,
            true,
        )
        .unwrap();
        assert_eq!(metrics.executions, 5);
        assert_eq!(metrics.execution_work, 43);
        assert_eq!(metrics.rates(1.0), (5.0, 40.0));
        assert_eq!(metrics.stop_reason, Some("execution_limit"));
        assert_eq!(metrics.extra["edge_attempts"], 7);
        assert_eq!(metrics.extra["budget_consumed"], 8);
        assert_eq!(metrics.extra["tasks_admitted"], 2);
        assert!(!objective_within_budget(
            metrics.work_to_first_objective,
            Some(39)
        ));
        assert!(objective_within_budget(
            metrics.work_to_first_objective,
            Some(40)
        ));
    }

    #[test]
    fn expansion_metric_work_overflow_never_saturates_to_a_complete_result() {
        let counters = ExpansionCounters {
            bootstrap_work: u64::MAX,
            reconstruction_work: 1,
            ..Default::default()
        };
        assert!(
            expansion_metrics(
                &counters,
                &ExpansionCounters::default(),
                ExpansionStopReason::Budget,
                false
            )
            .is_err()
        );
    }
    #[test]
    fn resumed_expansion_rates_exclude_the_checkpoint_prefix() {
        let initial = ExpansionCounters {
            executed_edges: 100,
            execution_work: 1000,
            reconstruction_work: 50,
            bootstrap_work: 7,
            tasks_reserved: 20,
            tasks_admitted: 18,
            edge_attempts: 102,
            budget_consumed: 103,
            ..Default::default()
        };
        let final_counts = ExpansionCounters {
            executed_edges: 120,
            execution_work: 1300,
            reconstruction_work: 70,
            bootstrap_work: 7,
            tasks_reserved: 22,
            tasks_admitted: 22,
            edge_attempts: 122,
            budget_consumed: 123,
            ..Default::default()
        };
        let metrics =
            expansion_metrics(&final_counts, &initial, ExpansionStopReason::Budget, false).unwrap();
        assert_eq!(metrics.executions, 120);
        assert_eq!(metrics.execution_work, 1377);
        assert_eq!(metrics.rates(2.0), (10.0, 160.0));
        assert_eq!(
            metrics.extra["session_counter_deltas"]["executed_edges"],
            20
        );
        assert_eq!(metrics.extra["session_counter_deltas"]["tasks_admitted"], 4);
        assert_eq!(
            metrics.extra["session_counter_deltas"]["bootstrap_frames"],
            0
        );
        assert_eq!(metrics.extra["initial_counters"]["bootstrap_work"], 7);
        let mut forged = final_counts;
        forged.executed_edges = 99;
        assert!(expansion_metrics(&forged, &initial, ExpansionStopReason::Budget, false).is_err());
    }
}
