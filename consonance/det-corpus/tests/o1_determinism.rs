// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 1 — O1 determinism, pass and **non-vacuous** fail.
//!
//! Positive: an arbitrary generated toy program at an arbitrary seed is
//! bit-identical across two runs, so `check_determinism` passes.
//!
//! Negative (the anti-vacuity part): `check_determinism` compares two machines
//! spawned from the *same* factory, so a [`FlakyFactory`] that perturbs *every*
//! spawn identically would yield two identical perturbed machines — a vacuous
//! pass. The factory must perturb **differently across spawns**.
//!
//! Mechanism: [`AlternatingFlakyFactory`] perturbs only **odd-numbered** spawns
//! (a spawn counter), leaving even spawns pristine. Both `compare_runs` and
//! `bisect_divergence` spawn machine A before machine B in fixed `(even, odd)`
//! pairs, so every comparison pits a clean (even) spawn against a perturbed
//! (odd) one. That makes the divergence **reproducible under the bisector's
//! from-scratch re-execution** — which a "only the literal second spawn ever
//! diverges" scheme would not be — and pins `first_divergent_work` to the exact
//! perturbation work count `T`.

use std::sync::atomic::{AtomicU64, Ordering};

use det_corpus::check_determinism;
use proptest::prelude::*;
use unison::MachineFactory;
use unison::flaky::{FlakyFactory, FlakyMachine, Perturbation};
use unison::toy::{ToyFactory, ToyMachine, generate_program};

/// Persistent divergence: XOR the PRNG state (a bijective update, so once two
/// states differ they differ forever — exactly what bisection needs).
const PERTURB: Perturbation = Perturbation::XorPrng {
    mask: 0x5EED_5EED_5EED_5EED,
};

/// Perturbs only odd-numbered spawns; even spawns use the `u64::MAX` "never"
/// sentinel and behave identically to the inner factory. See the module docs.
struct AlternatingFlakyFactory {
    inner: ToyFactory,
    diverge_at: u64,
    spawns: AtomicU64,
}

impl MachineFactory for AlternatingFlakyFactory {
    type M = FlakyMachine<ToyMachine>;

    fn spawn(&self, seed: u64) -> Self::M {
        let n = self.spawns.fetch_add(1, Ordering::Relaxed);
        let diverge_at = if n % 2 == 1 {
            self.diverge_at
        } else {
            u64::MAX
        };
        FlakyFactory {
            inner: self.inner.clone(),
            diverge_at,
            perturb: PERTURB,
        }
        .spawn(seed)
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Positive: two same-seed runs of a generated program are identical.
    #[test]
    fn deterministic_factory_passes_o1(
        prog_seed in any::<u64>(),
        seed in any::<u64>(),
        checkpoint_every in 8u64..=64,
    ) {
        let prog = generate_program(prog_seed, 256);
        let f = ToyFactory { program: prog.instrs };
        let limit = prog.work_to_halt + 16;
        let res = check_determinism(&f, seed, checkpoint_every, limit).unwrap();
        prop_assert!(res.passed, "expected pass, got {res:?}");
        prop_assert!(res.divergence.is_none());
    }

    /// Negative: an across-spawn-divergent factory fails, and the bisector pins
    /// the exact first-divergent work count `T`.
    #[test]
    fn across_spawn_divergence_localizes_t(
        prog_seed in any::<u64>(),
        seed in any::<u64>(),
        t in 1u64..=200,
        checkpoint_every in 8u64..=64,
    ) {
        let prog = generate_program(prog_seed, 256);
        // T is reachable: generate_program guarantees work_to_halt > 256 >= 257 > 200 >= t.
        prop_assume!(t < prog.work_to_halt);
        let f = AlternatingFlakyFactory {
            inner: ToyFactory { program: prog.instrs },
            diverge_at: t,
            spawns: AtomicU64::new(0),
        };
        let limit = prog.work_to_halt + 16;
        let res = check_determinism(&f, seed, checkpoint_every, limit).unwrap();
        prop_assert!(!res.passed, "expected fail, got {res:?}");
        let point = res.divergence.expect("bisection must localize the divergence");
        prop_assert_eq!(
            point.first_divergent_work, t,
            "bisector pinned {} but the perturbation fired at {}",
            point.first_divergent_work, t
        );
    }
}
