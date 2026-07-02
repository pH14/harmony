// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 3 — the **statistical gates**, all integer/fixed-point (≥256 cases
//! each; no `f32`/`f64` anywhere):
//!
//! - **(a) seed determinism** — the same seed yields an identical fault/no-fault
//!   decision sequence, driven end-to-end through [`RegimeTactic`].
//! - **(b) burstiness above IID** — over N-draw sequences at the *same exact
//!   mean rate* (an IID coin at `p = stationary_rate()`), the regime sequence's
//!   **windowed Fano factor** (count variance / count mean) exceeds the IID
//!   baseline's by a fixed margin. Compared by cross-multiplying the two
//!   integer ratios — no float.
//! - **(c) calibration** — the empirical fault rate over a long run is within a
//!   stated integer tolerance (5%) of the closed-form stationary rate.

use environment::{Answer, DecisionClass};
use explorer::{DecisionPoint, Prng, Tactic};
use proptest::prelude::*;
use tactics_regime::{RegimeProcess, RegimeTactic, class_tag};

/// The governed fault class the statistical sequences are drawn over.
const CLASS: DecisionClass = DecisionClass::BlockIo;

/// Drive a fresh regime (seeded from `regime_seed`) over `n` fault-class
/// decisions on a stream seeded from `stream_seed`, returning the fault/no-fault
/// bitstring. Each decision steps the chain then samples the active table — the
/// exact sequence [`RegimeTactic::decide`] answers, without the encode round-trip.
fn regime_sequence(regime_seed: u64, stream_seed: u64, n: usize) -> Vec<bool> {
    let mut proc = RegimeProcess::from_seed(regime_seed);
    let mut rng = Prng::new(stream_seed);
    (0..n)
        .map(|_| {
            proc.step(&mut rng);
            matches!(proc.sample(CLASS, &mut rng), Answer::Fault(_))
        })
        .collect()
}

/// An IID Bernoulli bitstring at exactly `p = num/den`, one PRNG word per draw
/// (`w % den < num`) — the equal-mean baseline.
fn iid_sequence(num: u64, den: u64, stream_seed: u64, n: usize) -> Vec<bool> {
    let mut rng = Prng::new(stream_seed);
    (0..n).map(|_| rng.next_u64() % den < num).collect()
}

/// The windowed Fano factor of a bitstring as an exact ratio `(numer, denom)`
/// with `Fano = (W·Q − T²) / (W·T)`, where `W` windows of size `w` hold
/// `c_1..c_W` faults, `T = Σc` and `Q = Σc²`. A bursty sequence concentrates
/// faults into some windows ⇒ high variance ⇒ Fano ≫ 1; an IID sequence ⇒ Fano
/// ≈ 1. Returns `None` if the sequence has no faults (`T = 0`, Fano undefined).
fn fano(seq: &[bool], w: usize) -> Option<(i128, i128)> {
    let windows = seq.len() / w;
    let mut t: i128 = 0;
    let mut q: i128 = 0;
    for k in 0..windows {
        let c = seq[k * w..(k + 1) * w].iter().filter(|&&b| b).count() as i128;
        t += c;
        q += c * c;
    }
    if t == 0 {
        return None;
    }
    let big_w = windows as i128;
    Some((big_w * q - t * t, big_w * t))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// (a) The same regime seed + same stream seed ⇒ the identical decision
    /// sequence, through the full `Tactic::decide` path.
    #[test]
    fn seed_determinism(regime_seed in any::<u64>(), stream_seed in any::<u64>()) {
        let run = || {
            let mut t = RegimeTactic::from_seed(regime_seed);
            let mut rng = Prng::new(stream_seed);
            (0..200u64)
                .map(|i| {
                    let pt = DecisionPoint {
                        at: explorer::Moment(i),
                        id: i,
                        ctx: class_tag(CLASS).to_le_bytes().to_vec(),
                    };
                    t.decide(&pt, &mut rng)
                })
                .collect::<Vec<_>>()
        };
        prop_assert_eq!(run(), run());
    }

    /// (b) The regime's windowed Fano factor exceeds the equal-mean IID
    /// baseline's by a fixed margin (Fano ≥ IID Fano + 1/2), cross-multiplied.
    #[test]
    fn burstiness_above_iid(regime_seed in any::<u64>(), stream_seed in any::<u64>()) {
        const N: usize = 8000;
        const W: usize = 16;

        let (rn, rd) = RegimeProcess::from_seed(regime_seed).stationary_rate();

        let regime = regime_sequence(regime_seed, stream_seed, N);
        // Equal-mean IID baseline at exactly the stationary rate.
        let iid = iid_sequence(rn, rd, stream_seed ^ 0x9E37_79B9, N);

        let (rf_n, rf_d) = fano(&regime, W).expect("bursty regime faults");
        // A degenerate zero-fault IID baseline is trivially less bursty.
        let (if_n, if_d) = fano(&iid, W).unwrap_or((0, 1));

        // regime_fano - iid_fano >= 1/2, i.e. 2·(rf_n·if_d - if_n·rf_d) >= rf_d·if_d.
        // All denominators are positive (W·T with T > 0).
        let lhs = 2 * (rf_n * if_d - if_n * rf_d);
        let rhs = rf_d * if_d;
        prop_assert!(
            lhs >= rhs,
            "regime Fano {}/{} must exceed IID Fano {}/{} by >= 1/2 \
             (stationary p = {}/{})",
            rf_n, rf_d, if_n, if_d, rn, rd
        );
    }

    /// (c) The empirical fault rate is within 5% of the stationary rate.
    #[test]
    fn calibration(regime_seed in any::<u64>(), stream_seed in any::<u64>()) {
        const N: usize = 40_000;
        let (rn, rd) = RegimeProcess::from_seed(regime_seed).stationary_rate();
        let seq = regime_sequence(regime_seed, stream_seed, N);
        let faults = seq.iter().filter(|&&b| b).count() as i128;

        // |faults/N - rn/rd| <= 1/20  ⇔  20·|faults·rd - rn·N| <= N·rd.
        let (rn, rd, n) = (rn as i128, rd as i128, N as i128);
        let deviation = (faults * rd - rn * n).abs();
        prop_assert!(
            20 * deviation <= n * rd,
            "empirical {}/{} strayed >5% from stationary {}/{}",
            faults, N, rn, rd
        );
    }
}
