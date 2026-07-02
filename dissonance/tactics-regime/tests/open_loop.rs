// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 2 — the **open-loop proptest** (≥256 cases, the task-64 pattern):
//! identical `(regime state, point, rng)` ⇒ identical answer, whatever else the
//! harness varies.
//!
//! A [`RegimeTactic`] is driven through a live point sequence with an unrelated
//! "noise" tactic decided in between; every `(point, stream-before, answer)` is
//! logged. Replaying the log through a *fresh* tactic — evolving the same regime
//! state over the same point sequence, fed each decision's logged stream state —
//! reproduces every answer byte-for-byte. Because `decide` takes only the point
//! and the stream (there is no other parameter through which feedback could
//! reach it), the noise cannot perturb it.

use explorer::{Answer, DecisionPoint, Prng, Tactic};
use proptest::prelude::*;
use tactics_regime::{RegimeTactic, class_tag};

use environment::DecisionClass;

/// A point carrying `class` in its `ctx` (the tactic's convention).
fn point(class: DecisionClass, id: u64) -> DecisionPoint {
    DecisionPoint {
        at: explorer::Moment(id),
        id,
        ctx: class_tag(class).to_le_bytes().to_vec(),
    }
}

/// The six classes cycled through, so both fault and supply classes appear.
const CLASSES: [DecisionClass; 6] = [
    DecisionClass::Entropy,
    DecisionClass::NetFlow,
    DecisionClass::Payload,
    DecisionClass::BlockIo,
    DecisionClass::Scheduler,
    DecisionClass::Process,
];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Replaying a logged decision stream through a fresh tactic reproduces
    /// every answer, even with an unrelated tactic decided between each step.
    #[test]
    fn tactic_is_open_loop(
        regime_seed in any::<u64>(),
        stream_seed in any::<u64>(),
        noise_seed in any::<u64>(),
        steps in 1u64..40,
    ) {
        // Live run: log (point, stream-before, answer) while a noise tactic
        // decides between each step to stand in for "whatever concurrent runs do."
        let mut live = RegimeTactic::from_seed(regime_seed);
        let mut noise = RegimeTactic::from_seed(noise_seed);
        let mut rng = Prng::new(stream_seed);
        let mut noise_rng = Prng::new(noise_seed ^ 0xABCD);

        let mut log: Vec<(DecisionPoint, Prng, Answer)> = Vec::new();
        for i in 0..steps {
            let pt = point(CLASSES[(i % 6) as usize], i);
            let before = rng.clone();
            let answer = live.decide(&pt, &mut rng);
            log.push((pt, before, answer));
            // Interleave an unrelated decision on a different tactic + stream.
            let np = point(CLASSES[((i + 3) % 6) as usize], i.wrapping_mul(7));
            noise.decide(&np, &mut noise_rng);
        }

        // Replay standalone: a fresh tactic, same regime seed, evolving over the
        // same point sequence, each decision fed its logged stream-before.
        let mut fresh = RegimeTactic::from_seed(regime_seed);
        for (pt, before, answer) in &log {
            let mut replay_rng = before.clone();
            prop_assert_eq!(
                &fresh.decide(pt, &mut replay_rng),
                answer,
                "identical (state, point, rng) must yield the identical answer"
            );
        }
    }
}
