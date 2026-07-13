// SPDX-License-Identifier: AGPL-3.0-or-later
//! Divergence injection: wrap a [`SubjectFactory`] so spawned machines are
//! perturbed once, the first time their work counter reaches a chosen
//! boundary. This simulates "run B has a nondeterminism bug at tick T" with T
//! known, so the bisector can be tested against ground truth.

use crate::{Subject, SubjectError, SubjectFactory, RunOutcome};
use serde::{Deserialize, Serialize};

/// A single one-shot state perturbation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Perturbation {
    /// XOR register `reg` (index mod 8) with `mask`. Note that later writes
    /// to the register can erase the divergence before a checkpoint observes
    /// it; prefer [`Perturbation::XorPrng`] when persistence is required.
    XorReg {
        /// Register index (taken mod 8 by the toy machine).
        reg: u8,
        /// XOR mask; 0 is a no-op.
        mask: u64,
    },
    /// XOR the PRNG state with `mask` (use a nonzero mask). The xorshift64*
    /// state update is a bijection, so two states that differ once differ at
    /// every later step — the divergence is permanent, which is what the
    /// bisector property tests rely on.
    XorPrng {
        /// XOR mask; 0 is a no-op.
        mask: u64,
    },
    /// Halt the machine immediately (produces [`crate::Verdict::HaltMismatch`]).
    ForceHalt,
}

/// Machines that know how to apply a [`Perturbation`] to their own
/// architectural state. [`Subject`] itself deliberately exposes no mutation
/// hooks, so divergence injection needs this extra capability.
pub trait Perturbable: Subject {
    /// Apply `p` to the current architectural state.
    fn apply_perturbation(&mut self, p: &Perturbation);
}

/// Wraps spawned machines so that the first time work reaches ≥ `diverge_at`,
/// `perturb` is applied exactly once. `diverge_at: u64::MAX` is the "never"
/// sentinel: the factory behaves identically to `inner`, unconditionally —
/// even for a machine whose work counter actually reaches `u64::MAX`.
///
/// Edge cases: `diverge_at: 0` perturbs at spawn (work starts at 0, which is
/// already ≥ 0); a machine that halts strictly before `diverge_at` is never
/// perturbed (the boundary is unreachable), but one that halts exactly *at*
/// `diverge_at` is.
#[derive(Debug, Clone)]
pub struct FlakyFactory<F: SubjectFactory> {
    /// Factory producing the machines to perturb.
    pub inner: F,
    /// Work count at which the perturbation fires.
    pub diverge_at: u64,
    /// What to do to the machine state at the boundary.
    pub perturb: Perturbation,
}

impl<F: SubjectFactory> SubjectFactory for FlakyFactory<F>
where
    F::M: Perturbable,
{
    type M = FlakyMachine<F::M>;

    fn spawn(&self, seed: u64) -> Self::M {
        let mut inner = self.inner.spawn(seed);
        let mut applied = false;
        if self.diverge_at != u64::MAX && inner.work() >= self.diverge_at {
            // Boundary already reached at spawn (diverge_at == 0).
            inner.apply_perturbation(&self.perturb);
            applied = true;
        }
        FlakyMachine {
            inner,
            diverge_at: self.diverge_at,
            perturb: self.perturb,
            applied,
        }
    }
}

/// A machine wrapper that applies its perturbation once, exactly when work
/// first reaches `diverge_at` — even when a `run_to` target lands beyond the
/// boundary (it runs to the boundary internally, perturbs, then continues).
#[derive(Debug, Clone)]
pub struct FlakyMachine<M: Perturbable> {
    inner: M,
    diverge_at: u64,
    perturb: Perturbation,
    applied: bool,
}

impl<M: Perturbable> Subject for FlakyMachine<M> {
    fn run_to(&mut self, target: u64) -> Result<RunOutcome, SubjectError> {
        if self.diverge_at == u64::MAX {
            // The "never" sentinel: behave identically to the inner machine,
            // even if its work counter legitimately reaches u64::MAX.
            return self.inner.run_to(target);
        }
        if !self.applied && self.inner.work() < self.diverge_at && target >= self.diverge_at {
            let outcome = self.inner.run_to(self.diverge_at)?;
            if outcome == RunOutcome::Halted && self.inner.work() < self.diverge_at {
                // Halted strictly before the boundary: the perturbation point
                // is unreachable, never apply it.
                return Ok(RunOutcome::Halted);
            }
            self.inner.apply_perturbation(&self.perturb);
            self.applied = true;
        }
        self.inner.run_to(target)
    }

    fn work(&self) -> u64 {
        self.inner.work()
    }

    fn state_hash(&self) -> [u8; 32] {
        self.inner.state_hash()
    }

    fn observable_digest(&self) -> [u8; 32] {
        // A perturbation is a state defect, not deliberate guest output: expose
        // the wrapped machine's observable digest unchanged.
        self.inner.observable_digest()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toy::{ToyFactory, asm, generate_program};

    const SEED: u64 = 1234;

    fn toy() -> ToyFactory {
        ToyFactory {
            program: generate_program(7, 2000).instrs,
        }
    }

    fn flaky(diverge_at: u64, perturb: Perturbation) -> FlakyFactory<ToyFactory> {
        FlakyFactory {
            inner: toy(),
            diverge_at,
            perturb,
        }
    }

    const XOR_R0: Perturbation = Perturbation::XorReg {
        reg: 0,
        mask: 0xFFFF_0000_FFFF_0000,
    };

    #[test]
    fn observable_digest_delegates_to_inner_and_varies_with_output() {
        // A RAND;OUT;HALT payload emits a seed-dependent value, so the wrapped
        // machine's observable_digest is a real digest that varies with output.
        let prog = vec![asm::rand(0), asm::out(0), asm::halt()];
        let mk = |seed: u64| {
            let f = FlakyFactory {
                inner: ToyFactory {
                    program: prog.clone(),
                },
                diverge_at: u64::MAX, // no-op wrapper
                perturb: XOR_R0,
            };
            let mut m = f.spawn(seed);
            m.run_to(100).unwrap();
            m
        };
        // Varies with the observed output ⇒ kills a constant [0;32]/[1;32] body.
        assert_ne!(mk(7).observable_digest(), mk(8).observable_digest());
        // Delegates to the inner machine's observable digest (the real contract).
        let mut inner = ToyFactory {
            program: prog.clone(),
        }
        .spawn(7);
        inner.run_to(100).unwrap();
        assert_eq!(mk(7).observable_digest(), inner.observable_digest());
    }

    #[test]
    fn perturbs_exactly_at_boundary_even_when_target_lands_beyond() {
        let f = flaky(100, XOR_R0);
        // Run straight past the boundary in one call.
        let mut m = f.spawn(SEED);
        m.run_to(100).unwrap();
        let hash_at_boundary = m.state_hash();

        // A clean machine run to the same point, then perturbed by hand,
        // must match: proves the perturbation fired at 100, not at 150.
        let mut clean = toy().spawn(SEED);
        clean.run_to(100).unwrap();
        assert_ne!(clean.state_hash(), hash_at_boundary);
        clean.apply_perturbation(&XOR_R0);
        assert_eq!(clean.state_hash(), hash_at_boundary);

        // And both continue identically afterwards.
        m.run_to(150).unwrap();
        clean.run_to(150).unwrap();
        assert_eq!(clean.state_hash(), m.state_hash());
    }

    #[test]
    fn clean_before_boundary_perturbed_at_it() {
        let f = flaky(100, XOR_R0);
        let clean = toy();
        let mut a = clean.spawn(SEED);
        let mut b = f.spawn(SEED);
        a.run_to(99).unwrap();
        b.run_to(99).unwrap();
        assert_eq!(a.state_hash(), b.state_hash());
        a.run_to(100).unwrap();
        b.run_to(100).unwrap();
        assert_ne!(a.state_hash(), b.state_hash());
    }

    #[test]
    fn perturbation_is_applied_only_once_and_path_independent() {
        let f = flaky(100, XOR_R0);
        // Many small run_to calls crossing the boundary...
        let mut a = f.spawn(SEED);
        for t in [30, 60, 99, 100, 101, 130, 700] {
            a.run_to(t).unwrap();
        }
        // ...must equal one big call.
        let mut b = f.spawn(SEED);
        b.run_to(700).unwrap();
        assert_eq!(a.state_hash(), b.state_hash());
    }

    #[test]
    fn max_diverge_at_behaves_identically_to_inner() {
        let f = flaky(u64::MAX, XOR_R0);
        let clean = toy();
        let mut a = clean.spawn(SEED);
        let mut b = f.spawn(SEED);
        let wa = a.run_to(u64::MAX).unwrap(); // runs to natural halt
        let wb = b.run_to(u64::MAX).unwrap();
        assert_eq!(wa, wb);
        assert_eq!(a.work(), b.work());
        assert_eq!(a.state_hash(), b.state_hash());
    }

    #[test]
    fn diverge_at_zero_perturbs_at_spawn() {
        let f = flaky(0, XOR_R0);
        let m = f.spawn(SEED);
        let mut clean = toy().spawn(SEED);
        assert_ne!(m.state_hash(), clean.state_hash());
        clean.apply_perturbation(&XOR_R0);
        assert_eq!(m.state_hash(), clean.state_hash());
    }

    #[test]
    fn halt_before_boundary_is_never_perturbed() {
        // Program halts at work 3; boundary at 10 is unreachable.
        let prog = vec![asm::loadi(0, 7), asm::out(0), asm::halt()];
        let f = FlakyFactory {
            inner: ToyFactory {
                program: prog.clone(),
            },
            diverge_at: 10,
            perturb: XOR_R0,
        };
        let mut m = f.spawn(SEED);
        assert_eq!(m.run_to(50).unwrap(), RunOutcome::Halted);
        assert_eq!(m.work(), 3);
        let mut clean = ToyFactory { program: prog }.spawn(SEED);
        clean.run_to(50).unwrap();
        assert_eq!(m.state_hash(), clean.state_hash());
        // Repeated calls stay clean (the boundary check re-runs harmlessly).
        assert_eq!(m.run_to(60).unwrap(), RunOutcome::Halted);
        assert_eq!(m.state_hash(), clean.state_hash());
    }

    #[test]
    fn halt_exactly_at_boundary_is_perturbed() {
        // Program halts at work 3 == diverge_at: work did reach the boundary.
        let prog = vec![asm::loadi(0, 7), asm::out(0), asm::halt()];
        let f = FlakyFactory {
            inner: ToyFactory {
                program: prog.clone(),
            },
            diverge_at: 3,
            perturb: XOR_R0,
        };
        let mut m = f.spawn(SEED);
        assert_eq!(m.run_to(50).unwrap(), RunOutcome::Halted);
        let mut clean = ToyFactory { program: prog }.spawn(SEED);
        clean.run_to(50).unwrap();
        assert_ne!(m.state_hash(), clean.state_hash());
        clean.apply_perturbation(&XOR_R0);
        assert_eq!(m.state_hash(), clean.state_hash());
    }

    #[test]
    fn force_halt_stops_at_the_boundary() {
        let f = flaky(137, Perturbation::ForceHalt);
        let mut m = f.spawn(SEED);
        assert_eq!(m.run_to(500).unwrap(), RunOutcome::Halted);
        assert_eq!(m.work(), 137);
    }

    /// Mock machine whose `run_to` jumps the work counter straight to the
    /// target, making `work == u64::MAX` actually reachable — the toy machine
    /// can't get there, but the trait permits it and the real-VM adapter is a
    /// foreign impl.
    struct JumpMachine {
        work: u64,
        perturbed: bool,
    }

    impl Subject for JumpMachine {
        fn run_to(&mut self, target: u64) -> Result<RunOutcome, SubjectError> {
            if target < self.work {
                return Err(SubjectError::TargetBehind {
                    target,
                    current: self.work,
                });
            }
            self.work = target;
            Ok(RunOutcome::ReachedTarget)
        }
        fn work(&self) -> u64 {
            self.work
        }
        fn state_hash(&self) -> [u8; 32] {
            let mut h = [0u8; 32];
            h[0] = u8::from(self.perturbed);
            h[1..9].copy_from_slice(&self.work.to_le_bytes());
            h
        }
    }

    impl Perturbable for JumpMachine {
        fn apply_perturbation(&mut self, _p: &Perturbation) {
            self.perturbed = true;
        }
    }

    struct JumpFactory {
        spawn_at: u64,
    }

    impl SubjectFactory for JumpFactory {
        type M = JumpMachine;
        fn spawn(&self, _seed: u64) -> JumpMachine {
            JumpMachine {
                work: self.spawn_at,
                perturbed: false,
            }
        }
    }

    #[test]
    fn max_sentinel_never_perturbs_even_at_work_u64_max() {
        // run_to path: the wrapped machine reaches work == u64::MAX.
        let f = FlakyFactory {
            inner: JumpFactory { spawn_at: 0 },
            diverge_at: u64::MAX,
            perturb: XOR_R0,
        };
        let mut m = f.spawn(SEED);
        let mut clean = JumpFactory { spawn_at: 0 }.spawn(SEED);
        assert_eq!(m.run_to(u64::MAX).unwrap(), RunOutcome::ReachedTarget);
        clean.run_to(u64::MAX).unwrap();
        assert_eq!(m.work(), u64::MAX);
        assert_eq!(
            m.state_hash(),
            clean.state_hash(),
            "u64::MAX sentinel must be an unconditional no-op"
        );

        // spawn path symmetry: a machine that already sits at u64::MAX.
        let f = FlakyFactory {
            inner: JumpFactory { spawn_at: u64::MAX },
            diverge_at: u64::MAX,
            perturb: XOR_R0,
        };
        let m = f.spawn(SEED);
        let clean = JumpFactory { spawn_at: u64::MAX }.spawn(SEED);
        assert_eq!(m.state_hash(), clean.state_hash());
    }

    #[test]
    fn sentinel_guard_is_narrow_sub_max_boundaries_still_fire() {
        // diverge_at just below the sentinel still perturbs, at the boundary.
        let f = FlakyFactory {
            inner: JumpFactory { spawn_at: 0 },
            diverge_at: u64::MAX - 1,
            perturb: XOR_R0,
        };
        let mut m = f.spawn(SEED);
        let mut clean = JumpFactory { spawn_at: 0 }.spawn(SEED);
        m.run_to(u64::MAX).unwrap();
        clean.run_to(u64::MAX).unwrap();
        assert_eq!(m.work(), u64::MAX);
        assert_ne!(m.state_hash(), clean.state_hash());
    }
}
