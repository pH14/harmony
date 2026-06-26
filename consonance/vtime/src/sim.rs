// SPDX-License-Identifier: AGPL-3.0-or-later
//! Test simulator: a [`CpuBackend`] over an abstract instruction stream with
//! seeded, adversarial PMU skid.
//!
//! [`SimCpu`] models the only two properties of a real vCPU + PMU that the
//! planner depends on:
//!
//! - **event density** — each instruction either is a counted event
//!   (advances work by 1) or is not (advances it by 0), decided by a seeded
//!   deterministic pattern with configurable density from 1.0 (every
//!   instruction counts) down to sparse (e.g. one counted event per 1000
//!   instructions);
//! - **overflow skid** — [`CpuBackend::run_until_overflow`] stops at an
//!   instruction boundary with `work = armed_at + skid`, where the skid
//!   sequence is a seeded xorshift64* PRNG drawing uniformly from
//!   `0..=max_skid` (skid in work units, never early).
//!
//! Every planner-visible interaction is recorded in an event log
//! ([`SimCpu::log`]) so tests can assert *how* the planner drove the CPU,
//! not just where it ended up.
//!
//! This module is public: other crates (the future perf_event backend's
//! tests, the VMM's replay tests) are expected to reuse it.

use crate::error::{BackendError, VtimeError};
use crate::planner::CpuBackend;

/// Configuration for a [`SimCpu`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimCpuConfig {
    /// Seed for both deterministic PRNGs (instruction pattern and skid
    /// sequence; the two streams are derived from it independently).
    /// A seed of 0 is mapped to a fixed non-zero constant (xorshift state
    /// must be non-zero).
    pub seed: u64,
    /// Event density numerator: each instruction is a counted event with
    /// probability `density_num / density_den`. Must satisfy
    /// `1 <= density_num <= density_den`.
    pub density_num: u64,
    /// Event density denominator. Must be non-zero.
    pub density_den: u64,
    /// Maximum overflow skid in work units; each
    /// [`CpuBackend::run_until_overflow`] draws uniformly from
    /// `0..=max_skid`.
    pub max_skid: u64,
    /// Work counter value at construction (0 for a fresh vCPU).
    pub initial_work: u64,
}

/// One planner-visible interaction, recorded by [`SimCpu`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimEvent {
    /// `run_until_overflow` was called with this armed work count.
    Armed {
        /// Absolute work count the overflow was armed at.
        armed_at: u64,
    },
    /// `run_until_overflow` returned.
    Stopped {
        /// The armed count of the matching [`SimEvent::Armed`].
        armed_at: u64,
        /// The skid drawn for this overflow (work units).
        skid: u64,
        /// Work count at which execution stopped (`armed_at + skid`, or the
        /// pre-call work count if that was already past it).
        stopped_at: u64,
    },
    /// `single_step` executed one instruction.
    Stepped {
        /// Whether the instruction was a counted event.
        counted: bool,
        /// Work count after the step.
        work_after: u64,
    },
}

/// Deterministic simulated vCPU + PMU implementing [`CpuBackend`].
#[derive(Debug, Clone)]
pub struct SimCpu {
    density_num: u64,
    density_den: u64,
    max_skid: u64,
    stream_rng: Xorshift64Star,
    skid_rng: Xorshift64Star,
    work: u64,
    instructions_retired: u64,
    log: Vec<SimEvent>,
}

impl SimCpu {
    /// Builds a simulator from a config.
    ///
    /// # Errors
    ///
    /// [`VtimeError::InvalidSimConfig`] if `density_den == 0`,
    /// `density_num == 0` (no instruction would ever advance work, so
    /// `run_until_overflow` could never terminate) or
    /// `density_num > density_den`.
    pub fn new(cfg: SimCpuConfig) -> Result<SimCpu, VtimeError> {
        if cfg.density_den == 0 {
            return Err(VtimeError::InvalidSimConfig("density_den must be non-zero"));
        }
        if cfg.density_num == 0 {
            return Err(VtimeError::InvalidSimConfig(
                "density_num must be non-zero (work would never advance)",
            ));
        }
        if cfg.density_num > cfg.density_den {
            return Err(VtimeError::InvalidSimConfig(
                "density_num must be <= density_den",
            ));
        }
        Ok(SimCpu {
            density_num: cfg.density_num,
            density_den: cfg.density_den,
            max_skid: cfg.max_skid,
            stream_rng: Xorshift64Star::new(cfg.seed),
            // Derive an independent skid stream from the same seed.
            skid_rng: Xorshift64Star::new(cfg.seed ^ 0x9E37_79B9_7F4A_7C15),
            work: cfg.initial_work,
            instructions_retired: 0,
            log: Vec::new(),
        })
    }

    /// The full event log of planner-visible interactions so far.
    pub fn log(&self) -> &[SimEvent] {
        &self.log
    }

    /// Total instructions retired so far, by both `single_step` and the
    /// free-running portion of `run_until_overflow`.
    pub fn instructions_retired(&self) -> u64 {
        self.instructions_retired
    }

    /// Models the hardware counter restarting at a snapshot restore: resets
    /// the work counter to 0 while the instruction stream (and the skid
    /// sequence) continue unchanged. Pair with rebuilding the `VClock` from
    /// [`crate::VClock::snapshot_vns`].
    pub fn reset_work_counter(&mut self) {
        self.work = 0;
    }

    /// Retires one instruction; returns whether it was a counted event.
    fn retire_one(&mut self) -> bool {
        self.instructions_retired += 1;
        let counted = (self.stream_rng.next_u64() % self.density_den) < self.density_num;
        if counted {
            self.work = self.work.saturating_add(1);
        }
        counted
    }

    /// Draws the next skid, uniform in `0..=max_skid`.
    fn draw_skid(&mut self) -> u64 {
        let raw = self.skid_rng.next_u64();
        if self.max_skid == u64::MAX {
            raw
        } else {
            raw % (self.max_skid + 1)
        }
    }
}

impl CpuBackend for SimCpu {
    fn work(&self) -> u64 {
        self.work
    }

    fn run_until_overflow(&mut self, armed_at: u64) -> Result<u64, BackendError> {
        self.log.push(SimEvent::Armed { armed_at });
        let skid = self.draw_skid();
        // Stop at the first instruction boundary where work reaches
        // armed_at + skid (>= armed_at: the PMU never fires early). If the
        // current work count is already past it, stop immediately — a real
        // counter armed at an already-passed count overflows at once.
        let stop_work = armed_at.saturating_add(skid).max(self.work);
        while self.work < stop_work {
            self.retire_one();
        }
        self.log.push(SimEvent::Stopped {
            armed_at,
            skid,
            stopped_at: self.work,
        });
        Ok(self.work)
    }

    fn single_step(&mut self) -> Result<u64, BackendError> {
        let counted = self.retire_one();
        self.log.push(SimEvent::Stepped {
            counted,
            work_after: self.work,
        });
        Ok(self.work)
    }
}

/// xorshift64* PRNG (Vigna): tiny, seedable, deterministic, integer-only.
#[derive(Debug, Clone)]
struct Xorshift64Star {
    state: u64,
}

impl Xorshift64Star {
    /// xorshift state must be non-zero; 0 is mapped to a fixed constant.
    fn new(seed: u64) -> Self {
        Xorshift64Star {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sim(seed: u64, density_num: u64, density_den: u64, max_skid: u64) -> SimCpu {
        SimCpu::new(SimCpuConfig {
            seed,
            density_num,
            density_den,
            max_skid,
            initial_work: 0,
        })
        .expect("valid test config")
    }

    #[test]
    fn rejects_bad_density() {
        let bad = [(0, 1), (1, 0), (3, 2)];
        for (num, den) in bad {
            let err = SimCpu::new(SimCpuConfig {
                seed: 1,
                density_num: num,
                density_den: den,
                max_skid: 0,
                initial_work: 0,
            })
            .unwrap_err();
            assert!(
                matches!(err, VtimeError::InvalidSimConfig(_)),
                "({num}, {den}): {err:?}"
            );
        }
    }

    #[test]
    fn density_one_counts_every_instruction() {
        let mut cpu = sim(7, 1, 1, 0);
        for expected in 1..=100u64 {
            assert_eq!(cpu.single_step().unwrap(), expected);
        }
        assert_eq!(cpu.instructions_retired(), 100);
    }

    #[test]
    fn same_seed_same_behavior() {
        let drive = |mut cpu: SimCpu| -> (u64, Vec<SimEvent>) {
            cpu.run_until_overflow(50).unwrap();
            for _ in 0..20 {
                cpu.single_step().unwrap();
            }
            (cpu.work(), cpu.log().to_vec())
        };
        let a = drive(sim(0xABCD, 1, 3, 5));
        let b = drive(sim(0xABCD, 1, 3, 5));
        assert_eq!(a, b);
        let c = drive(sim(0xABCE, 1, 3, 5));
        assert_ne!(a, c, "different seed should produce a different run");
    }

    #[test]
    fn zero_seed_is_usable() {
        let mut cpu = sim(0, 1, 2, 3);
        let stopped = cpu.run_until_overflow(10).unwrap();
        assert!(stopped >= 10);
    }

    #[test]
    fn overflow_respects_contract() {
        let mut cpu = sim(99, 1, 10, 7);
        for armed in [5u64, 20, 21, 100] {
            let stopped = cpu.run_until_overflow(armed).unwrap();
            assert!(stopped >= armed, "stopped {stopped} before armed {armed}");
            assert!(stopped <= armed + 7, "skid beyond max_skid");
            assert_eq!(stopped, cpu.work());
            let Some(SimEvent::Stopped {
                armed_at,
                skid,
                stopped_at,
            }) = cpu.log().last()
            else {
                panic!("expected a Stopped event");
            };
            assert_eq!((*armed_at, *stopped_at), (armed, stopped));
            assert_eq!(armed + skid, stopped);
        }
    }

    #[test]
    fn overflow_at_passed_count_stops_immediately() {
        let mut cpu = sim(3, 1, 1, 0);
        for _ in 0..50 {
            cpu.single_step().unwrap();
        }
        let retired_before = cpu.instructions_retired();
        let stopped = cpu.run_until_overflow(10).unwrap();
        assert_eq!(stopped, 50);
        assert_eq!(cpu.instructions_retired(), retired_before);
    }

    #[test]
    fn reset_work_counter_keeps_stream_state() {
        let mut a = sim(0x1234, 1, 3, 0);
        let mut b = sim(0x1234, 1, 3, 0);
        for _ in 0..30 {
            a.single_step().unwrap();
            b.single_step().unwrap();
        }
        let w = a.work();
        a.reset_work_counter();
        assert_eq!(a.work(), 0);
        // The instruction pattern continues identically, just rebased.
        for _ in 0..30 {
            let ra = a.single_step().unwrap();
            let rb = b.single_step().unwrap();
            assert_eq!(ra + w, rb);
        }
    }
}
