// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

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

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompletedBufferTelemetry {
    pub results_live: u64,
    pub results_peak: u64,
    pub actions_live: u64,
    pub actions_peak: u64,
    pub candidates_live: u64,
    pub candidates_peak: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct StreamTelemetry {
    pub command_capacity: usize,
    pub output_capacity: usize,
    pub batch_record_limit: usize,
    pub partial_batch_age_ns: u64,
    pub background_batches: u64,
    pub largest_batch_records: u64,
    pub background_records: u64,
    pub background_bytes: u64,
    pub encode_ns: u64,
    pub sink_ns: u64,
    pub flush_ns: u64,
    pub sink_bytes: u64,
    pub producer_backpressure_events: u64,
    pub producer_wait_ns: u64,
    pub records_in_flight_peak: u64,
    pub largest_buffer_capacity_bytes: u64,
    pub worker_cpu_ns: Option<u64>,
    pub worker_cpu_wait_ns: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CampaignTelemetry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<StreamTelemetry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_host: Option<HostTimes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_phase: Option<CoordinatorPhaseTelemetry>,
    pub bootstrap_ns: u64,
    pub search_ns: u64,
    pub persistence_ns: u64,
    pub coordinator: serde_json::Value,
    pub admission: AdmissionWaits,
    pub completed_buffer: CompletedBufferTelemetry,
    pub workers: Vec<WorkerTelemetry>,
    pub target: TargetCounters,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CoordinatorPhaseTelemetry {
    pub wall_ns: u64,
    pub host: Option<HostTimes>,
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

#[cfg(not(target_arch = "wasm32"))]
pub(crate) type Moment = std::time::Instant;

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Moment;

#[cfg(target_arch = "wasm32")]
impl Moment {
    pub(crate) const fn elapsed(self) -> std::time::Duration {
        std::time::Duration::ZERO
    }
}

pub(crate) fn nanos_since(started: Moment) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::disallowed_methods)]
pub(crate) fn now() -> Moment {
    std::time::Instant::now()
}

#[cfg(target_arch = "wasm32")]
pub(crate) const fn now() -> Moment {
    Moment
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::disallowed_methods)]
pub(crate) fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(target_arch = "wasm32")]
pub(crate) const fn unix_seconds() -> u64 {
    0
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
        Self::try_measure(coordinator_start, idle).unwrap_or(Self {
            idle_admission_order_ns: idle.0,
            idle_no_job_ns: idle.1,
            ..Self::default()
        })
    }

    pub(crate) fn try_measure(
        coordinator_start: Option<(u64, u64)>,
        idle: (u64, u64),
    ) -> Option<Self> {
        let (cpu_before, wait_before) = coordinator_start?;
        let (cpu, wait) = thread_schedstat()?;
        Some(Self {
            coordinator_cpu_ns: cpu.saturating_sub(cpu_before),
            coordinator_run_wait_ns: wait.saturating_sub(wait_before),
            idle_admission_order_ns: idle.0,
            idle_no_job_ns: idle.1,
        })
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
    since: Moment,
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
    fn unavailable_coordinator_cpu_is_distinct_from_a_measured_zero() {
        assert_eq!(HostTimes::try_measure(None, (3, 4)), None);
        let mut telemetry = CampaignTelemetry::default();
        assert!(
            serde_json::to_value(&telemetry)
                .unwrap()
                .get("coordinator_host")
                .is_none()
        );
        telemetry.coordinator_host = Some(HostTimes::default());
        assert_eq!(
            serde_json::to_value(&telemetry).unwrap()["coordinator_host"]["coordinator_cpu_ns"],
            0
        );
    }

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
