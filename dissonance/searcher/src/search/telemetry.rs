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
    pub target: TargetCounters,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdmissionWaits {
    pub receive_all_running_ns: u64,
    pub receive_head_of_line_ns: u64,
    pub results_held: u64,
    pub results_held_ns: u64,
    pub results_held_max_ns: u64,
    pub idle_admission_order_ns: u64,
    pub idle_no_job_ns: u64,
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

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct HostTimes {
    pub coordinator_cpu_ns: u64,
    pub coordinator_run_wait_ns: u64,
    pub idle_admission_order_ns: u64,
    pub idle_no_job_ns: u64,
}

impl HostTimes {
    pub(crate) fn measure(coordinator_start: Option<(u64, u64)>, idle: (u64, u64)) -> Self {
        let (coordinator_cpu_ns, coordinator_run_wait_ns) = coordinator_start
            .zip(thread_schedstat())
            .map_or((0, 0), |((cpu_before, wait_before), (cpu, wait))| {
                (
                    cpu.saturating_sub(cpu_before),
                    wait.saturating_sub(wait_before),
                )
            });
        Self {
            coordinator_cpu_ns,
            coordinator_run_wait_ns,
            idle_admission_order_ns: idle.0,
            idle_no_job_ns: idle.1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IdleReason {
    AdmissionOrder,
    NoJob,
}

pub(crate) struct IdleClock {
    workers: usize,
    busy: usize,
    reason: IdleReason,
    since: Instant,
    admission_order_ns: u64,
    no_job_ns: u64,
}

impl IdleClock {
    pub(crate) fn new(workers: usize) -> Self {
        Self {
            workers,
            busy: 0,
            reason: IdleReason::NoJob,
            since: now(),
            admission_order_ns: 0,
            no_job_ns: 0,
        }
    }

    pub(crate) fn update(&mut self, outstanding: usize, reason: IdleReason) {
        (self.admission_order_ns, self.no_job_ns) = self.totals();
        self.since = now();
        self.busy = outstanding.min(self.workers);
        self.reason = reason;
    }

    pub(crate) fn totals(&self) -> (u64, u64) {
        let idle = u64::try_from(self.workers.saturating_sub(self.busy)).unwrap_or(u64::MAX);
        let charged = nanos_since(self.since).saturating_mul(idle);
        match self.reason {
            IdleReason::AdmissionOrder => (
                self.admission_order_ns.saturating_add(charged),
                self.no_job_ns,
            ),
            IdleReason::NoJob => (
                self.admission_order_ns,
                self.no_job_ns.saturating_add(charged),
            ),
        }
    }

    pub(crate) fn finish(mut self, waits: &mut AdmissionWaits) {
        self.update(0, IdleReason::NoJob);
        waits.idle_admission_order_ns = self.admission_order_ns;
        waits.idle_no_job_ns = self.no_job_ns;
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
    fn idle_worker_time_is_charged_to_the_reason_recorded_when_workers_went_idle() {
        let mut clock = IdleClock::new(3);
        clock.update(3, IdleReason::NoJob);
        clock.update(1, IdleReason::AdmissionOrder);
        std::thread::sleep(std::time::Duration::from_millis(2));
        clock.update(3, IdleReason::NoJob);
        let mut waits = AdmissionWaits::default();
        clock.finish(&mut waits);
        assert!(waits.idle_admission_order_ns >= 4_000_000);
        assert!(waits.idle_no_job_ns < waits.idle_admission_order_ns);
    }

    #[test]
    fn live_idle_totals_include_the_interval_still_open() {
        let mut clock = IdleClock::new(2);
        clock.update(0, IdleReason::AdmissionOrder);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let (admission_order, no_job) = clock.totals();
        assert!(admission_order >= 4_000_000);
        assert_eq!(no_job, clock.no_job_ns);
        let mut waits = AdmissionWaits::default();
        clock.finish(&mut waits);
        assert!(waits.idle_admission_order_ns >= admission_order);
    }
}
