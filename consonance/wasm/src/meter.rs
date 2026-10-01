// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::admission::{AdmissionError, Profile};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Meter {
    supplied: u64,
    service_cost: u64,
}
impl Meter {
    pub const QUANTUM: u64 = 1024;
    pub const MAXIMUM: u64 = 1 << 60;
    pub fn rounded_deadline(deadline: u64) -> Result<u64, AdmissionError> {
        if deadline > Self::MAXIMUM {
            return Err(AdmissionError(
                "deadline exceeds the execution clock".into(),
            ));
        }
        deadline
            .checked_add(Self::QUANTUM - 1)
            .map(|value| value / Self::QUANTUM * Self::QUANTUM)
            .ok_or_else(|| AdmissionError("deadline rounding overflow".into()))
    }
    pub fn grant(&mut self, remaining: u64) -> Result<u64, AdmissionError> {
        self.validate(remaining)?;
        let supplied = self
            .supplied
            .checked_add(Self::QUANTUM)
            .filter(|value| {
                value
                    .checked_add(self.service_cost)
                    .is_some_and(|total| total <= Self::MAXIMUM)
            })
            .ok_or_else(|| AdmissionError("execution clock exhausted".into()))?;
        let fuel = remaining
            .checked_add(Self::QUANTUM)
            .ok_or_else(|| AdmissionError("fuel overflow".into()))?;
        self.supplied = supplied;
        Ok(fuel)
    }
    pub fn moment(&self, remaining: u64) -> Result<u64, AdmissionError> {
        self.validate(remaining)?;
        (self.supplied - remaining)
            .checked_add(self.service_cost)
            .filter(|at| *at <= Self::MAXIMUM)
            .ok_or_else(|| AdmissionError("execution clock overflow".into()))
    }
    pub fn charge_service(
        &mut self,
        request_bytes: usize,
        answer_bytes: usize,
    ) -> Result<(), AdmissionError> {
        let bytes = request_bytes
            .checked_add(answer_bytes)
            .ok_or_else(|| AdmissionError("service cost overflow".into()))?;
        let cost = 64u64
            .checked_add(bytes as u64)
            .ok_or_else(|| AdmissionError("service cost overflow".into()))?;
        let total = self
            .service_cost
            .checked_add(cost)
            .filter(|value| {
                value
                    .checked_add(self.supplied)
                    .is_some_and(|total| total <= Self::MAXIMUM)
            })
            .ok_or_else(|| AdmissionError("service clock exhausted".into()))?;
        self.service_cost = total;
        Ok(())
    }
    pub fn validate(&self, remaining: u64) -> Result<(), AdmissionError> {
        if remaining > self.supplied
            || self.supplied > Self::MAXIMUM
            || self.service_cost > Self::MAXIMUM
            || self
                .supplied
                .checked_add(self.service_cost)
                .is_none_or(|total| total > Self::MAXIMUM)
            || !self.supplied.is_multiple_of(Self::QUANTUM)
        {
            return Err(AdmissionError("invalid cumulative fuel state".into()));
        }
        Ok(())
    }
    pub fn maximum_deadline_overshoot(profile: &Profile) -> u64 {
        u64::from(profile.memory_pages) * 65536 / 64
            + u64::from(profile.maximum_function_bytes) * 16
            + Self::QUANTUM
            + 64
            + 2 * hypercall_proto::MAX_PAYLOAD as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rounding_and_failed_accounting_are_atomic() {
        assert_eq!(Meter::rounded_deadline(0).unwrap(), 0);
        assert_eq!(Meter::rounded_deadline(1).unwrap(), 1024);
        assert_eq!(Meter::rounded_deadline(1024).unwrap(), 1024);
        assert_eq!(Meter::rounded_deadline(1025).unwrap(), 2048);
        assert!(Meter::rounded_deadline(u64::MAX).is_err());
        let mut meter = Meter::default();
        let before = meter.clone();
        assert!(meter.grant(1).is_err());
        assert_eq!(meter, before);
        let mut remaining = meter.grant(0).unwrap();
        remaining -= 31;
        meter.charge_service(4, 8).unwrap();
        assert_eq!(meter.moment(remaining).unwrap(), 107);
        let bytes = postcard::to_allocvec(&meter).unwrap();
        let restored: Meter = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(restored.moment(remaining).unwrap(), 107);
        assert!(meter.charge_service(usize::MAX, 1).is_err());
        assert_eq!(meter, restored);
    }
}

#[derive(Clone, Debug)]
pub struct Cancellation {
    pub(crate) flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl Cancellation {
    pub fn cancel(&self) {
        self.flag.store(true, std::sync::atomic::Ordering::Release);
    }
}

#[derive(Debug, Default)]
pub struct ExecutionControl {
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    progress: std::sync::Arc<std::sync::atomic::AtomicU64>,
    abandoned: bool,
}
impl ExecutionControl {
    pub fn cancellation(&self) -> Cancellation {
        Cancellation {
            flag: self.cancel.clone(),
        }
    }
    pub fn progress(&self) -> std::sync::Arc<std::sync::atomic::AtomicU64> {
        self.progress.clone()
    }
    pub fn check(&mut self) -> Result<(), AdmissionError> {
        self.abandoned |= self.cancel.load(std::sync::atomic::Ordering::Acquire);
        if self.abandoned {
            return Err(AdmissionError("canceled execution cannot resume".into()));
        }
        Ok(())
    }
    pub fn record_progress(&self, moment: u64) {
        self.progress
            .store(moment, std::sync::atomic::Ordering::Release);
    }
    pub fn abandoned(&self) -> bool {
        self.abandoned || self.cancel.load(std::sync::atomic::Ordering::Acquire)
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    #[test]
    fn cancel_is_irrevocable_even_after_the_latch_is_cleared() {
        let mut control = ExecutionControl::default();
        control.record_progress(17);
        assert_eq!(
            control
                .progress()
                .load(std::sync::atomic::Ordering::Acquire),
            17
        );
        let latch = control.cancellation();
        latch.cancel();
        assert!(control.check().is_err());
        latch
            .flag
            .store(false, std::sync::atomic::Ordering::Release);
        assert!(control.check().is_err());
        assert!(control.abandoned());
    }
}
