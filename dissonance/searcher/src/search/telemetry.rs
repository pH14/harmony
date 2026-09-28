// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeMap, time::Instant};

use serde::{Deserialize, Serialize};

pub type TargetCounters = BTreeMap<String, u64>;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkerTelemetry {
    pub boot_ns: u64,
    pub jobs: u64,
    pub busy_ns: u64,
    pub idle_ns: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_ns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_wait_ns: Option<u64>,
    pub idle_admission_order_ns: u64,
    pub idle_no_job_ns: u64,
    pub target: TargetCounters,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdmissionWaits {
    pub receive_all_running_ns: u64,
    pub receive_head_of_line_ns: u64,
    pub results_held: u64,
    pub results_held_ns: u64,
    pub results_held_max_ns: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CampaignTelemetry {
    pub bootstrap_ns: u64,
    pub search_ns: u64,
    pub persistence_ns: u64,
    pub coordinator: serde_json::Value,
    pub admission: AdmissionWaits,
    pub workers: Vec<WorkerTelemetry>,
    pub target: TargetCounters,
}

impl PartialEq for CampaignTelemetry {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for CampaignTelemetry {}

impl CampaignTelemetry {
    pub(crate) fn sum_targets(&mut self) {
        self.target.clear();
        for worker in &self.workers {
            for (name, value) in &worker.target {
                let total = self.target.entry(name.clone()).or_default();
                *total = total.saturating_add(*value);
            }
        }
    }
}

pub(crate) fn nanos_since(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

#[allow(clippy::disallowed_methods)]
pub(crate) fn now() -> Instant {
    Instant::now()
}

pub(crate) fn thread_schedstat() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/thread-self/schedstat").ok()?;
    let mut fields = text.split_ascii_whitespace();
    let cpu = fields.next()?.parse().ok()?;
    let wait = fields.next()?.parse().ok()?;
    Some((cpu, wait))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IdleReason {
    AdmissionOrder,
    NoJob,
}

pub(crate) struct IdleClock {
    since: Vec<Option<(Instant, IdleReason)>>,
    admission_order_ns: Vec<u64>,
    no_job_ns: Vec<u64>,
}

impl IdleClock {
    pub(crate) fn new(workers: usize) -> Self {
        let started = now();
        Self {
            since: vec![Some((started, IdleReason::NoJob)); workers],
            admission_order_ns: vec![0; workers],
            no_job_ns: vec![0; workers],
        }
    }

    pub(crate) fn idle(&mut self, worker: usize, reason: IdleReason) {
        if let Some(slot) = self.since.get_mut(worker) {
            *slot = Some((now(), reason));
        }
    }

    pub(crate) fn busy(&mut self, worker: usize) {
        let Some((since, reason)) = self.since.get_mut(worker).and_then(Option::take) else {
            return;
        };
        let elapsed = nanos_since(since);
        let total = match reason {
            IdleReason::AdmissionOrder => &mut self.admission_order_ns[worker],
            IdleReason::NoJob => &mut self.no_job_ns[worker],
        };
        *total = total.saturating_add(elapsed);
    }

    pub(crate) fn finish(mut self, workers: &mut [WorkerTelemetry]) {
        for worker in 0..self.since.len() {
            self.busy(worker);
        }
        for (index, worker) in workers.iter_mut().enumerate() {
            worker.idle_admission_order_ns =
                self.admission_order_ns.get(index).copied().unwrap_or(0);
            worker.idle_no_job_ns = self.no_job_ns.get(index).copied().unwrap_or(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_counters_sum_across_workers() {
        let mut telemetry = CampaignTelemetry {
            workers: vec![
                WorkerTelemetry {
                    target: BTreeMap::from([("a".to_owned(), 2), ("b".to_owned(), 1)]),
                    ..WorkerTelemetry::default()
                },
                WorkerTelemetry {
                    target: BTreeMap::from([("a".to_owned(), 3)]),
                    ..WorkerTelemetry::default()
                },
            ],
            ..CampaignTelemetry::default()
        };
        telemetry.sum_targets();
        assert_eq!(
            telemetry.target,
            BTreeMap::from([("a".to_owned(), 5), ("b".to_owned(), 1)])
        );
    }

    #[test]
    fn idle_time_is_charged_to_the_reason_recorded_when_the_worker_went_idle() {
        let mut clock = IdleClock::new(2);
        clock.busy(0);
        clock.busy(1);
        clock.idle(0, IdleReason::AdmissionOrder);
        std::thread::sleep(std::time::Duration::from_millis(2));
        clock.busy(0);
        clock.busy(0);
        let mut workers = vec![WorkerTelemetry::default(); 2];
        clock.finish(&mut workers);
        assert!(workers[0].idle_admission_order_ns >= 2_000_000);
        assert_eq!(workers[1].idle_admission_order_ns, 0);
    }
}
