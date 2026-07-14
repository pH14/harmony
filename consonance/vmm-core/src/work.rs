// SPDX-License-Identifier: AGPL-3.0-or-later
//! The V-time **work source** — the seam between the deterministic clock and the
//! host counter of retired conditional branches.
//!
//! V-time is a pure function of *work performed*: `work` = retired conditional
//! branches, read at every VM exit ([`vtime`] crate docs). This module defines
//! the [`WorkSource`] trait the run loop reads at each exit and feeds into
//! [`vtime::VClock`] (so RDTSC = `VClock::tsc(work)`), plus the portable
//! [`ScriptedWork`] used by the unit/property tests. The real, box-only
//! `perf_event` counter (`BR_INST_RETIRED.CONDITIONAL`, guest-only, pinned) is
//! [`crate::vendor::x86::work_perf::PerfWorkCounter`].
//!
//! **Layering (R-Backend).** The work source lives **above** the `Backend`
//! trait, in the run loop, and is the *same* regardless of which backend is in
//! use — `perf_event` attaches to the vCPU thread, not to KVM-the-substrate. So
//! nothing here branches on the backend, and the backend never reads a counter
//! (a `VClock::tsc` call inside `vmm-backend` would be a layering bug). The
//! boundary (work source in vmm-core's loop, not behind the trait) is the one
//! task-21 P3 left to choose; see `IMPLEMENTATION.md`.

/// A failure reading or resetting the work counter (the box `perf_event` fd; the
/// portable sources are infallible).
#[derive(Debug, thiserror::Error)]
pub enum WorkError {
    /// The underlying counter syscall failed (carries the OS error).
    #[error("work-counter io error: {0}")]
    Io(#[from] std::io::Error),
    /// The pinned counter failed to schedule or was multiplexed — its read is
    /// not a trustworthy guest-branch count, so it is rejected rather than used.
    #[error("work-counter not trustworthy: {0}")]
    Untrustworthy(&'static str),
}

/// A monotonic source of *work* (cumulative retired guest conditional branches)
/// read at each VM exit. The run loop turns the value into guest-visible time
/// via [`vtime::VClock`].
///
/// Contract: [`WorkSource::work`] is **non-decreasing** between [`reset`]s, and
/// [`reset`] returns the count to `0` (snapshot restore: the hardware counter
/// restarts at 0 and the restored clock carries the effective V-time in its
/// `vns_base`, INTEGRATION.md §4). It is **not** required to advance on every
/// call — two reads with no counted event between them return the same value
/// (so two back-to-back RDTSCs read the same TSC; strict monotonicity needs a
/// branch between them).
///
/// [`reset`]: WorkSource::reset
pub trait WorkSource {
    /// The current cumulative work count (retired guest conditional branches
    /// since the last [`reset`](WorkSource::reset)).
    ///
    /// # Errors
    /// The box `perf_event` read can fail or report a multiplexed counter; the
    /// portable sources never error.
    fn work(&self) -> Result<u64, WorkError>;

    /// Reset the count to `0` (snapshot restore). After this, [`work`] counts
    /// from zero again.
    ///
    /// # Errors
    /// The box `perf_event` ioctl can fail; the portable sources never error.
    ///
    /// [`work`]: WorkSource::work
    fn reset(&mut self) -> Result<(), WorkError>;

    /// Prepare the counter for a fresh run — called by the run loop
    /// ([`Vmm::run`](crate::vmm::Vmm::run)) immediately **before the first guest
    /// entry**, so [`work`](WorkSource::work) thereafter counts only **this** run's
    /// guest execution.
    ///
    /// This matters for the box [`PerfWorkCounter`](crate::vendor::x86::work_perf::PerfWorkCounter):
    /// it is enabled at open and counts guest branches on the (CPU-pinned, but
    /// **shared**) vCPU thread, so a counter opened before a *coexisting* VM runs
    /// would otherwise accumulate that VM's branches. That breaks
    /// [`unison::compare_runs`], which spawns **both** machines and *then* runs both:
    /// the second machine's counter, opened before the first ran, would include the
    /// first's work and the two same-seed runs would diverge in their work-derived
    /// V-time. The box source overrides this to clear that accumulation; the
    /// spawn→run→spawn→run ordering (a single machine, or the bisector's probe) is
    /// unaffected either way.
    ///
    /// Default **no-op**: the portable [`ScriptedWork`] is per-instance and starts
    /// clean — a test that pre-loads a value via [`ScriptedWork::at`] keeps it.
    ///
    /// # Errors
    /// The box `perf_event` ioctl can fail; the portable sources never error.
    ///
    /// [`work`]: WorkSource::work
    fn start_run(&mut self) -> Result<(), WorkError> {
        Ok(())
    }
}

/// A deterministic, in-process [`WorkSource`] for unit/property tests: the work
/// count is whatever the test sets, advanced explicitly. Lets the V-time
/// completion logic (RDTSC = `VClock::tsc(work)`, monotonicity, snapshot
/// continuity) be exercised on every platform with no `perf_event`.
#[derive(Debug, Default, Clone)]
pub struct ScriptedWork {
    work: u64,
}

impl ScriptedWork {
    /// A source starting at work `0`.
    pub fn new() -> Self {
        Self { work: 0 }
    }

    /// A source starting at `work`.
    pub fn at(work: u64) -> Self {
        Self { work }
    }

    /// Advance the work count by `delta` (saturating), modelling `delta` retired
    /// conditional branches between two exits.
    pub fn advance(&mut self, delta: u64) -> &mut Self {
        self.work = self.work.saturating_add(delta);
        self
    }

    /// Set the absolute work count.
    pub fn set(&mut self, work: u64) -> &mut Self {
        self.work = work;
        self
    }
}

impl WorkSource for ScriptedWork {
    fn work(&self) -> Result<u64, WorkError> {
        Ok(self.work)
    }

    fn reset(&mut self) -> Result<(), WorkError> {
        self.work = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_advances_resets_and_saturates() {
        let mut w = ScriptedWork::new();
        assert_eq!(w.work().unwrap(), 0);
        w.advance(3).advance(4);
        assert_eq!(w.work().unwrap(), 7);
        w.set(100);
        assert_eq!(w.work().unwrap(), 100);
        w.advance(u64::MAX);
        assert_eq!(w.work().unwrap(), u64::MAX); // saturating, never wraps
        w.reset().unwrap();
        assert_eq!(w.work().unwrap(), 0);
    }

    #[test]
    fn scripted_at_starts_offset() {
        let w = ScriptedWork::at(42);
        assert_eq!(w.work().unwrap(), 42);
    }

    /// `WorkSource` is object-safe (held as `Box<dyn WorkSource>` in `Vmm`).
    #[test]
    fn work_source_is_object_safe() {
        let mut w: Box<dyn WorkSource> = Box::new(ScriptedWork::at(5));
        assert_eq!(w.work().unwrap(), 5);
        w.reset().unwrap();
        assert_eq!(w.work().unwrap(), 0);
    }

    /// The default `start_run` is a no-op: a portable source that pre-loaded a value
    /// (`ScriptedWork::at`) keeps it across a run-start, so the existing V-time
    /// completion tests (work N ⇒ tsc 2N) are unaffected. The box `PerfWorkCounter`
    /// overrides it to clear cross-VM thread accumulation.
    #[test]
    fn default_start_run_is_a_no_op() {
        let mut w = ScriptedWork::at(42);
        w.start_run().unwrap();
        assert_eq!(
            w.work().unwrap(),
            42,
            "default start_run must not disturb work"
        );
    }
}
