// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{RunOutcome, Subject, SubjectError, SubjectFactory};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Perturbation {
    XorReg { reg: u8, mask: u64 },
    XorPrng { mask: u64 },
    ForceHalt,
}

pub trait Perturbable: Subject {
    fn apply_perturbation(&mut self, p: &Perturbation);
}

#[derive(Debug, Clone)]
pub struct FlakyFactory<F: SubjectFactory> {
    pub inner: F,
    pub diverge_at: u64,
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
            return self.inner.run_to(target);
        }
        if !self.applied && self.inner.work() < self.diverge_at && target >= self.diverge_at {
            let outcome = self.inner.run_to(self.diverge_at)?;
            if outcome == RunOutcome::Halted && self.inner.work() < self.diverge_at {
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

    fn state_hash(&self) -> Result<[u8; 32], SubjectError> {
        self.inner.state_hash()
    }

    fn observable_digest(&self) -> [u8; 32] {
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
        let prog = vec![asm::rand(0), asm::out(0), asm::halt()];
        let mk = |seed: u64| {
            let f = FlakyFactory {
                inner: ToyFactory {
                    program: prog.clone(),
                },
                diverge_at: u64::MAX,
                perturb: XOR_R0,
            };
            let mut m = f.spawn(seed);
            m.run_to(100).unwrap();
            m
        };
        assert_ne!(mk(7).observable_digest(), mk(8).observable_digest());
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
        let mut m = f.spawn(SEED);
        m.run_to(100).unwrap();
        let hash_at_boundary = m.state_hash().unwrap();

        let mut clean = toy().spawn(SEED);
        clean.run_to(100).unwrap();
        assert_ne!(clean.state_hash().unwrap(), hash_at_boundary);
        clean.apply_perturbation(&XOR_R0);
        assert_eq!(clean.state_hash().unwrap(), hash_at_boundary);

        m.run_to(150).unwrap();
        clean.run_to(150).unwrap();
        assert_eq!(clean.state_hash().unwrap(), m.state_hash().unwrap());
    }

    #[test]
    fn clean_before_boundary_perturbed_at_it() {
        let f = flaky(100, XOR_R0);
        let clean = toy();
        let mut a = clean.spawn(SEED);
        let mut b = f.spawn(SEED);
        a.run_to(99).unwrap();
        b.run_to(99).unwrap();
        assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
        a.run_to(100).unwrap();
        b.run_to(100).unwrap();
        assert_ne!(a.state_hash().unwrap(), b.state_hash().unwrap());
    }

    #[test]
    fn perturbation_is_applied_only_once_and_path_independent() {
        let f = flaky(100, XOR_R0);
        let mut a = f.spawn(SEED);
        for t in [30, 60, 99, 100, 101, 130, 700] {
            a.run_to(t).unwrap();
        }
        let mut b = f.spawn(SEED);
        b.run_to(700).unwrap();
        assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
    }

    #[test]
    fn max_diverge_at_behaves_identically_to_inner() {
        let f = flaky(u64::MAX, XOR_R0);
        let clean = toy();
        let mut a = clean.spawn(SEED);
        let mut b = f.spawn(SEED);
        let wa = a.run_to(u64::MAX).unwrap();
        let wb = b.run_to(u64::MAX).unwrap();
        assert_eq!(wa, wb);
        assert_eq!(a.work(), b.work());
        assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
    }

    #[test]
    fn diverge_at_zero_perturbs_at_spawn() {
        let f = flaky(0, XOR_R0);
        let m = f.spawn(SEED);
        let mut clean = toy().spawn(SEED);
        assert_ne!(m.state_hash().unwrap(), clean.state_hash().unwrap());
        clean.apply_perturbation(&XOR_R0);
        assert_eq!(m.state_hash().unwrap(), clean.state_hash().unwrap());
    }

    #[test]
    fn halt_before_boundary_is_never_perturbed() {
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
        assert_eq!(m.state_hash().unwrap(), clean.state_hash().unwrap());
        assert_eq!(m.run_to(60).unwrap(), RunOutcome::Halted);
        assert_eq!(m.state_hash().unwrap(), clean.state_hash().unwrap());
    }

    #[test]
    fn halt_exactly_at_boundary_is_perturbed() {
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
        assert_ne!(m.state_hash().unwrap(), clean.state_hash().unwrap());
        clean.apply_perturbation(&XOR_R0);
        assert_eq!(m.state_hash().unwrap(), clean.state_hash().unwrap());
    }

    #[test]
    fn force_halt_stops_at_the_boundary() {
        let f = flaky(137, Perturbation::ForceHalt);
        let mut m = f.spawn(SEED);
        assert_eq!(m.run_to(500).unwrap(), RunOutcome::Halted);
        assert_eq!(m.work(), 137);
    }

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
        fn state_hash(&self) -> Result<[u8; 32], SubjectError> {
            let mut h = [0u8; 32];
            h[0] = u8::from(self.perturbed);
            h[1..9].copy_from_slice(&self.work.to_le_bytes());
            Ok(h)
        }
        fn observable_digest(&self) -> [u8; 32] {
            self.state_hash().unwrap()
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
            m.state_hash().unwrap(),
            clean.state_hash().unwrap(),
            "u64::MAX sentinel must be an unconditional no-op"
        );

        let f = FlakyFactory {
            inner: JumpFactory { spawn_at: u64::MAX },
            diverge_at: u64::MAX,
            perturb: XOR_R0,
        };
        let m = f.spawn(SEED);
        let clean = JumpFactory { spawn_at: u64::MAX }.spawn(SEED);
        assert_eq!(m.state_hash().unwrap(), clean.state_hash().unwrap());
    }

    #[test]
    fn sentinel_guard_is_narrow_sub_max_boundaries_still_fire() {
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
        assert_ne!(m.state_hash().unwrap(), clean.state_hash().unwrap());
    }
}
