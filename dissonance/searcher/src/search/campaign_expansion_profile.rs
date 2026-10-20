// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

#[derive(Default, Serialize)]
pub(super) struct ExpansionProfile {
    pub reserve_ns: u128,
    pub dispatch_ns: u128,
    pub selection_ns: u128,
    pub origin_ns: u128,
    pub complete_ns: u128,
    pub admit_ns: u128,
    pub validation_ns: u128,
    pub evidence_ns: u128,
    pub archive_ns: u128,
    pub maintenance_ns: u128,
    pub watch_ns: u128,
    pub record_ns: u128,
    pub stream_ns: u128,
    pub progress_ns: u128,
    pub checkpoint_ns: u128,
    pub drain_ns: u128,
    pub persistence_poll_ns: u128,
    pub stream_finish_ns: u128,
}

pub(super) fn record_profile(total: &mut u128, started: Option<Moment>) {
    if let Some(started) = started {
        *total = total.saturating_add(started.elapsed().as_nanos());
    }
}

#[derive(Serialize)]
pub(super) struct ExpansionCpuProfile {
    pub dispatch_cpu_ns: Option<u128>,
    pub receive_cpu_ns: Option<u128>,
    pub receive_calls: u64,
    pub receive_timeouts: u64,
    pub receive_replies: u64,
    pub receive_errors: u64,
    pub receive_cpu_samples: u64,
    pub counter_overflow: bool,
}

impl ExpansionCpuProfile {
    pub fn new(enabled: bool) -> Self {
        Self {
            dispatch_cpu_ns: None,
            receive_cpu_ns: (enabled && cfg!(target_os = "linux")).then_some(0),
            receive_calls: 0,
            receive_timeouts: 0,
            receive_replies: 0,
            receive_errors: 0,
            receive_cpu_samples: 0,
            counter_overflow: false,
        }
    }

    pub fn record_dispatch(&mut self, before: Option<u128>, after: Option<u128>) {
        self.dispatch_cpu_ns = cpu_delta(before, after);
    }

    pub fn record_receive(
        &mut self,
        before: Option<u128>,
        after: Option<u128>,
        status: ReceiveStatus,
    ) {
        increment_counter(&mut self.receive_calls, &mut self.counter_overflow);
        let count = match status {
            ReceiveStatus::Reply => &mut self.receive_replies,
            ReceiveStatus::Timeout => &mut self.receive_timeouts,
            ReceiveStatus::Error => &mut self.receive_errors,
        };
        increment_counter(count, &mut self.counter_overflow);
        let delta = cpu_delta(before, after);
        if delta.is_some() {
            increment_counter(&mut self.receive_cpu_samples, &mut self.counter_overflow);
        }
        self.receive_cpu_ns = if self.counter_overflow {
            None
        } else {
            self.receive_cpu_ns
                .zip(delta)
                .and_then(|(total, delta)| total.checked_add(delta))
        };
    }
}

pub(super) enum ReceiveStatus {
    Reply,
    Timeout,
    Error,
}

fn increment_counter(value: &mut u64, overflow: &mut bool) {
    if let Some(next) = value.checked_add(1) {
        *value = next;
    } else {
        *overflow = true;
    }
}

fn cpu_delta(before: Option<u128>, after: Option<u128>) -> Option<u128> {
    after?.checked_sub(before?)
}

#[cfg(any(target_os = "linux", test))]
fn timespec_ns(seconds: i64, nanoseconds: i64) -> Option<u128> {
    if !(0..1_000_000_000).contains(&nanoseconds) {
        return None;
    }
    u128::try_from(seconds)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(u128::try_from(nanoseconds).ok()?)
}

fn read_profile_cpu(enabled: bool, read: impl FnOnce() -> Option<u128>) -> Option<u128> {
    enabled.then(read).flatten()
}

pub(super) fn profile_cpu_now() -> Option<u128> {
    read_profile_cpu(profile_clock_enabled(), || {
        #[cfg(target_os = "linux")]
        {
            let sample = rustix::time::clock_gettime(rustix::time::ClockId::ThreadCPUTime);
            timespec_ns(sample.tv_sec, sample.tv_nsec)
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expansion_cpu_clock_does_not_read_when_disabled() {
        assert_eq!(
            read_profile_cpu(false, || panic!("unexpected clock read")),
            None
        );
        assert_eq!(read_profile_cpu(true, || Some(17)), Some(17));
    }

    #[test]
    fn expansion_cpu_conversion_and_delta_reject_invalid_samples() {
        assert_eq!(timespec_ns(2, 9), Some(2_000_000_009));
        assert_eq!(timespec_ns(-1, 0), None);
        assert_eq!(timespec_ns(0, -1), None);
        assert_eq!(timespec_ns(0, 1_000_000_000), None);
        assert_eq!(cpu_delta(Some(4), Some(9)), Some(5));
        assert_eq!(cpu_delta(Some(9), Some(4)), None);
        assert_eq!(cpu_delta(None, Some(4)), None);
        assert_eq!(cpu_delta(Some(4), None), None);
    }

    #[test]
    fn expansion_cpu_receive_coverage_retains_gaps_and_classifies_calls() {
        let mut value = ExpansionCpuProfile::new(true);
        value.receive_cpu_ns = Some(0);
        value.record_receive(Some(2), Some(8), ReceiveStatus::Reply);
        value.record_receive(Some(9), Some(10), ReceiveStatus::Timeout);
        assert_eq!(value.receive_cpu_ns, Some(7));
        value.record_receive(None, Some(11), ReceiveStatus::Error);
        value.record_receive(Some(12), Some(13), ReceiveStatus::Reply);
        assert_eq!(value.receive_cpu_ns, None);
        assert_eq!(value.receive_calls, 4);
        assert_eq!(value.receive_cpu_samples, 3);
        assert_eq!(value.receive_replies, 2);
        assert_eq!(value.receive_timeouts, 1);
        assert_eq!(value.receive_errors, 1);
        value.receive_cpu_ns = Some(u128::MAX);
        value.record_receive(Some(1), Some(2), ReceiveStatus::Timeout);
        assert_eq!(value.receive_cpu_ns, None);
        value.receive_calls = u64::MAX;
        value.receive_cpu_ns = Some(0);
        value.record_receive(Some(1), Some(2), ReceiveStatus::Reply);
        assert_eq!(value.receive_calls, u64::MAX);
        assert!(value.counter_overflow);
        assert_eq!(value.receive_cpu_ns, None);
        value.record_dispatch(Some(12), Some(11));
        assert_eq!(value.dispatch_cpu_ns, None);
    }
}
