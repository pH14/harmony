// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`RegimeProcess`] — the two-state calm/storm Markov chain that makes fault
//! entropy bursty, and the [`RegimeParams`] "swizzle" knob drawn once per run
//! from the seed. Integer/fixed-point throughout: every probability is a
//! `num/den` rational (the [`FaultPolicy`](environment::FaultPolicy) idiom) and
//! no float ever reaches state (crate invariant (b)).

use environment::Answer;
use environment::{DecisionClass, Fault};
use explorer::Prng;

use thiserror::Error;

/// The largest denominator [`RegimeParams::new`] admits for any probability. It
/// bounds the operands of [`RegimeParams::stationary_rate`]'s closed form so the
/// exact rational always fits a `(u64, u64)` after reduction (a `4096`-step
/// probability granularity is far finer than the calm/storm contrast needs).
pub const DEN_CAP: u64 = 1 << 12;

/// Why a [`RegimeParams`]/[`StateTable`] could not be built: an out-of-range
/// probability, or an eligible fault that does not belong to its class. The
/// library never panics on bad parameters (conventions rule 4).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Error)]
pub enum RegimeError {
    /// A probability denominator was `0` or exceeded [`DEN_CAP`].
    #[error("probability denominator must be in 1..={DEN_CAP}")]
    BadDenominator,
    /// A probability numerator exceeded its denominator (a probability `> 1`).
    #[error("probability numerator must not exceed its denominator")]
    NumeratorTooLarge,
    /// An eligible fault did not belong to the class it was filed under.
    #[error("an eligible fault does not belong to its fault class")]
    WrongClass,
    /// A **point-relative** fault (whose admissibility needs the live decision
    /// point's bounds — only `BlockTorn(n)`, which requires `n <= len`) was placed
    /// in a table. An open-loop tactic never sees those bounds (the spine
    /// `DecisionPoint`'s `ctx` is opaque and carries no request length), so it
    /// cannot guarantee such a fault admissible and would risk aborting a valid
    /// short-request point. Refused at construction so **every** emitted fault is
    /// unconditionally admissible for its class.
    #[error("a point-relative fault (e.g. BlockTorn) cannot be emitted by a context-free tactic")]
    UnboundableFault,
}

/// One regime of the two-state chain.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Regime {
    /// The low-intensity baseline: faults are rare.
    Calm,
    /// A dwell period of elevated, clustered fault probability.
    Storm,
}

impl Regime {
    /// The other regime — the destination of a transition.
    fn flipped(self) -> Self {
        match self {
            Self::Calm => Self::Storm,
            Self::Storm => Self::Calm,
        }
    }
}

/// A validated `num/den` probability in `[0, 1]` with `1 <= den <= DEN_CAP`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Prob {
    num: u64,
    den: u64,
}

impl Prob {
    /// Validate a probability, rejecting a zero/oversized denominator or a
    /// numerator that would make it exceed `1`.
    fn new(num: u64, den: u64) -> Result<Self, RegimeError> {
        if den == 0 || den > DEN_CAP {
            return Err(RegimeError::BadDenominator);
        }
        if num > den {
            return Err(RegimeError::NumeratorTooLarge);
        }
        Ok(Self { num, den })
    }

    /// A fixed-point Bernoulli trial on one PRNG word (`w % den < num`), the
    /// exact [`ClassPolicy`](environment::FaultPolicy) idiom. `den >= 1` is an
    /// invariant, so the modulo never divides by zero.
    fn trial(self, w: u64) -> bool {
        w % self.den < self.num
    }
}

/// One regime's per-class fault table — [`FaultPolicy`](environment::FaultPolicy)-shaped:
/// a single fixed-point fault probability applied to whichever fault class
/// surfaces, plus the eligible faults to pick from per class. A storm is simply
/// this table with an elevated probability; the eligible lists say *which* fault,
/// the probability says *how often*.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StateTable {
    prob: Prob,
    net: Vec<Fault>,
    block: Vec<Fault>,
    process: Vec<Fault>,
}

impl StateTable {
    /// Build a table faulting with probability `num/den`, drawing from the given
    /// per-class eligible lists. Each list is validated to hold only faults of
    /// its class and is canonicalized (sorted, deduplicated) so the table's
    /// behavior never depends on input order.
    pub fn new(
        num: u64,
        den: u64,
        net: &[Fault],
        block: &[Fault],
        process: &[Fault],
    ) -> Result<Self, RegimeError> {
        let prob = Prob::new(num, den)?;
        Ok(Self {
            prob,
            net: canonical(net, DecisionClass::NetFlow)?,
            block: canonical(block, DecisionClass::BlockIo)?,
            process: canonical(process, DecisionClass::Process)?,
        })
    }

    /// A table at probability `num/den` with the crate's default eligible lists
    /// (a representative, parameter-free fault per class from the enforced guest
    /// catalog) — the ergonomic constructor the seed-drawn params use.
    pub fn uniform(num: u64, den: u64) -> Result<Self, RegimeError> {
        Self::new(
            num,
            den,
            &default_net(),
            &default_block(),
            &default_process(),
        )
    }

    /// This table's fault probability, as `(num, den)`.
    pub fn probability(&self) -> (u64, u64) {
        (self.prob.num, self.prob.den)
    }

    /// The eligible list for a fault class (`None` for a supply class).
    fn eligible(&self, class: DecisionClass) -> Option<&[Fault]> {
        match class {
            DecisionClass::NetFlow => Some(&self.net),
            DecisionClass::BlockIo => Some(&self.block),
            DecisionClass::Process => Some(&self.process),
            _ => None,
        }
    }

    /// Draw one guest [`Answer`] for a fault-class decision, advancing `rng` by
    /// exactly one word (so a fault-class decision advances the stream uniformly,
    /// independent of outcome — the [`ClassPolicy`](environment::FaultPolicy)
    /// contract). The word decides both the Bernoulli trial and the eligible
    /// index. A supply class (or an empty eligible list) answers
    /// [`Answer::Nominal`] without perturbation. Every fault it can emit is
    /// bound-free (point-relative faults are refused at construction — see
    /// [`RegimeError::UnboundableFault`]), so the result is **always admissible**
    /// for its class at any decision point.
    fn sample(&self, class: DecisionClass, rng: &mut Prng) -> Answer {
        let w = rng.next_u64();
        match self.eligible(class) {
            Some(eligible) if self.prob.trial(w) && !eligible.is_empty() => {
                let idx = ((w / self.prob.den) % eligible.len() as u64) as usize;
                Answer::Fault(eligible[idx])
            }
            _ => Answer::Nominal,
        }
    }
}

/// Canonicalize an eligible list: reject any foreign-class or point-relative
/// fault, then sort and deduplicate so the bytes/behavior are order-independent.
fn canonical(faults: &[Fault], class: DecisionClass) -> Result<Vec<Fault>, RegimeError> {
    for f in faults {
        if f.class() != class {
            return Err(RegimeError::WrongClass);
        }
        if is_point_relative(*f) {
            return Err(RegimeError::UnboundableFault);
        }
    }
    let mut v = faults.to_vec();
    v.sort_unstable();
    v.dedup();
    Ok(v)
}

/// Whether a fault's admissibility depends on the live decision point's bounds.
/// Only [`Fault::BlockTorn`] does (`n <= len` against the request length); every
/// other guest fault is bound-free (its admissibility is class-match only). A
/// context-free tactic can guarantee bound-free faults admissible but not
/// point-relative ones, so [`canonical`] refuses the latter.
fn is_point_relative(f: Fault) -> bool {
    matches!(f, Fault::BlockTorn(_))
}

/// The default net eligible list (a full drop and a reset — the two convergent
/// flow-kill policies).
fn default_net() -> Vec<Fault> {
    vec![Fault::NetReset, Fault::NetLoss { num: 1, den: 1 }]
}

/// The default block eligible list (`EIO` and `ENOSPC`).
fn default_block() -> Vec<Fault> {
    vec![Fault::BlockEio, Fault::BlockNospc]
}

/// The default process eligible list (kill and restart).
fn default_process() -> Vec<Fault> {
    vec![Fault::ProcKill, Fault::ProcRestart]
}

/// The regime knobs, drawn **once per run** from the seed (FoundationDB swizzle):
/// the two transition probabilities and the calm/storm per-class fault tables. A
/// distinct run draws a distinct regime, so the campaign explores a spread of
/// storm intensities and durations while any single run stays autocorrelated.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RegimeParams {
    /// `P(Calm -> Storm)` per step (a small probability ⇒ long calm dwells).
    p_calm_to_storm: Prob,
    /// `P(Storm -> Calm)` per step (a small probability ⇒ long, bursty storms —
    /// the geometric dwell time that gives the autocorrelation).
    p_storm_to_calm: Prob,
    /// The calm table (low fault probability).
    calm: StateTable,
    /// The storm table (elevated fault probability).
    storm: StateTable,
}

impl RegimeParams {
    /// Build validated params from explicit transition probabilities and tables.
    /// Every probability must satisfy `1 <= den <= DEN_CAP` and `num <= den`;
    /// otherwise a [`RegimeError`] is returned (never a panic).
    pub fn new(
        p_calm_to_storm: (u64, u64),
        p_storm_to_calm: (u64, u64),
        calm: StateTable,
        storm: StateTable,
    ) -> Result<Self, RegimeError> {
        Ok(Self {
            p_calm_to_storm: Prob::new(p_calm_to_storm.0, p_calm_to_storm.1)?,
            p_storm_to_calm: Prob::new(p_storm_to_calm.0, p_storm_to_calm.1)?,
            calm,
            storm,
        })
    }

    /// Draw a **clustered** regime from `seed` (the swizzle knob). Storm
    /// intensity and dwell are drawn from discrete clusters so distinct runs land
    /// in distinct regimes, and every draw is guaranteed *meaningfully bursty*:
    /// the storm fault probability strictly dominates the calm one and both
    /// dwell times span several steps, so this arm is always the bursty arm (the
    /// quiet/IID baseline is a different tactic — task 70/72's portfolio).
    pub fn from_seed(seed: u64) -> Self {
        // Domain-separated so the params stream never coincides with a campaign
        // stream seeded by the same number.
        let mut rng = Prng::new(seed ^ 0x5357_495A_5A4C_4521); // "SWIZZLE!"

        // Dwell clusters: a storm lasts on average 1/p_storm_to_calm steps, a
        // calm spell 1/p_calm_to_storm. Pick a cluster, then jitter within it —
        // clustered, but never degenerate. Storms are always **several** steps
        // long (dwell >= 8) so faults land adjacent within a storm — the
        // autocorrelation the burstiness gate detects.
        const STORM_DWELL: [u64; 4] = [8, 12, 20, 30];
        const CALM_DWELL: [u64; 4] = [16, 40, 90, 180];
        let storm_dwell = STORM_DWELL[(rng.next_u64() % 4) as usize] + rng.next_u64() % 4;
        let calm_dwell = CALM_DWELL[(rng.next_u64() % 4) as usize] + rng.next_u64() % 16;

        // Intensity clusters: storms fault a large fraction (>= 1/2), calm a tiny
        // one. The storm numerator is clustered high; calm stays a small fixed
        // floor, so storm probability strictly dominates calm by design and every
        // storm is a genuine burst.
        const STORM_NUM: [u64; 4] = [2048, 2560, 3072, 3584];
        let storm_num =
            (STORM_NUM[(rng.next_u64() % 4) as usize] + rng.next_u64() % 256).min(DEN_CAP);
        let calm_num = 1 + rng.next_u64() % 8;

        // Transition probability = 1/dwell; fault probability = num/DEN_CAP.
        // All operands are <= DEN_CAP, so `new` is infallible here — but we fall
        // back to a safe default rather than unwrap, keeping the crate panic-free.
        Self::new(
            (1, calm_dwell),
            (1, storm_dwell),
            StateTable::uniform(calm_num, DEN_CAP).unwrap_or_else(|_| safe_table(1)),
            StateTable::uniform(storm_num.min(DEN_CAP), DEN_CAP).unwrap_or_else(|_| safe_table(1)),
        )
        .unwrap_or_else(|_| Self {
            p_calm_to_storm: Prob { num: 1, den: 64 },
            p_storm_to_calm: Prob { num: 1, den: 8 },
            calm: safe_table(1),
            storm: safe_table(DEN_CAP / 2),
        })
    }

    /// The exact stationary mean fault rate as a reduced rational `(num, den)` —
    /// the closed form for a two-state chain, so a gate can build an equal-mean
    /// IID baseline coin. With `p = P(C->S)`, `q = P(S->C)`, and per-state fault
    /// probabilities `f_calm`, `f_storm`, the stationary mix is
    /// `π_storm = p/(p+q)`, `π_calm = q/(p+q)`, and the rate is
    /// `π_calm·f_calm + π_storm·f_storm`. Computed in `u128` over a common
    /// denominator, then reduced by `gcd`; [`DEN_CAP`] bounds every operand so
    /// the result always fits `(u64, u64)`.
    ///
    /// **Frozen chain.** When both transition numerators are zero (`p == q == 0`,
    /// which [`new`](Self::new) accepts) the chain is non-ergodic: it never leaves
    /// its start state, which is always [`Calm`](Regime::Calm) (see
    /// [`RegimeProcess::new`]). The `p + q` denominator would then be zero, so
    /// this is special-cased to the **calm** table's probability — the exact
    /// long-run rate the frozen process actually exhibits — rather than a
    /// degenerate `0` (which would silently hand the statistical gates a wrong
    /// baseline).
    pub fn stationary_rate(&self) -> (u64, u64) {
        let p = &self.p_calm_to_storm;
        let q = &self.p_storm_to_calm;
        // Put p, q over the common denominator p.den * q.den: the stationary
        // weights are then integers `p' = p.num*q.den` (storm) and
        // `q' = q.num*p.den` (calm).
        let storm_w = (p.num as u128) * (q.den as u128);
        let calm_w = (q.num as u128) * (p.den as u128);
        let sum = storm_w + calm_w;

        let (cn, cd) = self.calm.probability();
        let (sn, sd) = self.storm.probability();
        let (cn, cd, sn, sd) = (cn as u128, cd as u128, sn as u128, sd as u128);

        if sum == 0 {
            // Frozen in the Calm start state: the true rate is the calm table's.
            return reduce(cn, cd);
        }

        // rate = (calm_w·cn/cd + storm_w·sn/sd) / sum
        //      = (calm_w·cn·sd + storm_w·sn·cd) / (sum·cd·sd)
        let num = calm_w * cn * sd + storm_w * sn * cd;
        let den = sum * cd * sd;
        reduce(num, den)
    }

    /// The calm table.
    pub fn calm(&self) -> &StateTable {
        &self.calm
    }

    /// The storm table.
    pub fn storm(&self) -> &StateTable {
        &self.storm
    }
}

/// A never-failing fallback table (`num/DEN_CAP`, default eligible lists),
/// keeping [`RegimeParams::from_seed`] panic-free even if a future edit to the
/// cluster tables were to violate a bound.
fn safe_table(num: u64) -> StateTable {
    StateTable::new(
        num.min(DEN_CAP),
        DEN_CAP,
        &default_net(),
        &default_block(),
        &default_process(),
    )
    // The operands are in range by construction, so this cannot fail; a manual
    // fallback keeps the crate free of `.unwrap()` on a fallible boundary.
    .unwrap_or(StateTable {
        prob: Prob { num: 0, den: 1 },
        net: Vec::new(),
        block: Vec::new(),
        process: Vec::new(),
    })
}

/// The running two-state chain: params plus the current regime, advanced one
/// step per surfaced fault decision.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RegimeProcess {
    params: RegimeParams,
    state: Regime,
}

impl RegimeProcess {
    /// Start a chain in [`Calm`](Regime::Calm) from the given params.
    pub fn new(params: RegimeParams) -> Self {
        Self {
            params,
            state: Regime::Calm,
        }
    }

    /// Start a chain from a seed-drawn regime (the per-run swizzle draw).
    pub fn from_seed(seed: u64) -> Self {
        Self::new(RegimeParams::from_seed(seed))
    }

    /// The current regime.
    pub fn state(&self) -> Regime {
        self.state
    }

    /// The params backing this chain.
    pub fn params(&self) -> &RegimeParams {
        &self.params
    }

    /// The stationary mean fault rate (delegates to [`RegimeParams::stationary_rate`]).
    pub fn stationary_rate(&self) -> (u64, u64) {
        self.params.stationary_rate()
    }

    /// Advance the chain one step, drawing one PRNG word for the transition
    /// Bernoulli trial (the current regime's out-probability).
    pub fn step(&mut self, rng: &mut Prng) {
        let p = match self.state {
            Regime::Calm => self.params.p_calm_to_storm,
            Regime::Storm => self.params.p_storm_to_calm,
        };
        if p.trial(rng.next_u64()) {
            self.state = self.state.flipped();
        }
    }

    /// Sample the **active** state's table for a fault-class decision, drawing
    /// one PRNG word. Does not advance the chain — a caller
    /// ([`RegimeTactic`](crate::RegimeTactic)) steps first, then samples.
    pub fn sample(&self, class: DecisionClass, rng: &mut Prng) -> Answer {
        match self.state {
            Regime::Calm => self.params.calm.sample(class, rng),
            Regime::Storm => self.params.storm.sample(class, rng),
        }
    }
}

/// Reduce a `u128` rational to lowest terms and narrow to `(u64, u64)`. The
/// caller guarantees (via [`DEN_CAP`]) the reduced terms fit `u64`; a defensive
/// clamp keeps it total rather than panicking if that guarantee were ever
/// violated.
fn reduce(num: u128, den: u128) -> (u64, u64) {
    if den == 0 {
        return (0, 1);
    }
    let g = gcd(num, den).max(1);
    let (n, d) = (num / g, den / g);
    (
        n.min(u64::MAX as u128) as u64,
        d.clamp(1, u64::MAX as u128) as u64,
    )
}

/// Binary-free Euclidean gcd on `u128`.
fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A seed-drawn regime is always meaningfully bursty: storm fault
    /// probability strictly dominates calm, and both dwell times exceed one step
    /// (geometric dwell). Pinned across many seeds.
    #[test]
    fn from_seed_is_always_bursty() {
        for seed in 0u64..2000 {
            let p = RegimeParams::from_seed(seed);
            let (cn, cd) = p.calm().probability();
            let (sn, sd) = p.storm().probability();
            // storm prob > calm prob, cross-multiplied (no floats).
            assert!(
                (sn as u128) * (cd as u128) > (cn as u128) * (sd as u128),
                "seed {seed}: storm must strictly dominate calm"
            );
            // Transition dwell >= 2 steps each (den >= 2 for a 1/den prob).
            assert!(p.p_calm_to_storm.den >= 2 && p.p_storm_to_calm.den >= 2);
        }
    }

    /// The stationary rate lies strictly between the calm and storm intensities
    /// (a convex mix) and is returned in lowest terms.
    #[test]
    fn stationary_rate_is_a_reduced_convex_mix() {
        let params = RegimeParams::new(
            (1, 64),
            (1, 8),
            StateTable::uniform(2, DEN_CAP).unwrap(),
            StateTable::uniform(2048, DEN_CAP).unwrap(),
        )
        .unwrap();
        let (n, d) = params.stationary_rate();
        assert_eq!(gcd(n as u128, d as u128), 1, "returned in lowest terms");
        // calm = 2/4096 < rate < storm = 2048/4096, cross-multiplied.
        assert!((n as u128) * 4096 > 2 * (d as u128), "rate exceeds calm");
        assert!((n as u128) * 4096 < 2048 * (d as u128), "rate below storm");
    }

    /// Exact stationary value for a hand-computed chain: p=1/2, q=1/2 ⇒
    /// π_calm=π_storm=1/2; f_calm=0, f_storm=1 ⇒ rate = 1/2.
    #[test]
    fn stationary_rate_exact_hand_value() {
        let params = RegimeParams::new(
            (1, 2),
            (1, 2),
            StateTable::uniform(0, DEN_CAP).unwrap(),
            StateTable::uniform(DEN_CAP, DEN_CAP).unwrap(),
        )
        .unwrap();
        assert_eq!(params.stationary_rate(), (1, 2));
    }

    /// A frozen chain (both transition numerators zero) never leaves its Calm
    /// start state, so the rate is the calm table's probability — not a
    /// degenerate `0`. Pins the `sum == 0` special case.
    #[test]
    fn stationary_rate_frozen_chain_is_calm_rate() {
        let params = RegimeParams::new(
            (0, 4),
            (0, 8),
            StateTable::uniform(3, DEN_CAP).unwrap(),
            StateTable::uniform(DEN_CAP, DEN_CAP).unwrap(),
        )
        .unwrap();
        // calm = 3/4096, already in lowest terms.
        assert_eq!(params.stationary_rate(), (3, DEN_CAP));
        // And a frozen chain that also never faults in Calm reads as exactly 0/1.
        let zero = RegimeParams::new(
            (0, 4),
            (0, 8),
            StateTable::uniform(0, DEN_CAP).unwrap(),
            StateTable::uniform(DEN_CAP, DEN_CAP).unwrap(),
        )
        .unwrap();
        assert_eq!(zero.stationary_rate(), (0, 1));
    }

    /// Bad parameters are rejected, never panic.
    #[test]
    fn new_rejects_out_of_range() {
        assert_eq!(
            StateTable::uniform(5, 0).unwrap_err(),
            RegimeError::BadDenominator
        );
        assert_eq!(
            StateTable::uniform(2, DEN_CAP + 1).unwrap_err(),
            RegimeError::BadDenominator
        );
        assert_eq!(
            StateTable::uniform(DEN_CAP + 1, DEN_CAP).unwrap_err(),
            RegimeError::NumeratorTooLarge
        );
        assert_eq!(
            StateTable::new(1, 2, &[Fault::BlockEio], &[], &[]).unwrap_err(),
            RegimeError::WrongClass
        );
    }

    /// A point-relative fault (`BlockTorn`, whose admissibility needs the request
    /// length) is refused at construction — a context-free tactic cannot emit it
    /// without risking an abort at a short-request point. Bound-free block faults
    /// are accepted.
    #[test]
    fn new_rejects_point_relative_faults() {
        assert_eq!(
            StateTable::new(1, 2, &[], &[Fault::BlockTorn(4096)], &[]).unwrap_err(),
            RegimeError::UnboundableFault
        );
        assert!(
            StateTable::new(1, 2, &[], &[Fault::BlockEio, Fault::BlockNospc], &[]).is_ok(),
            "bound-free block faults are accepted"
        );
    }

    /// `step` flips exactly when the transition trial fires; a `1/1` out-prob
    /// always flips, a `0/1` never does.
    #[test]
    fn step_transitions_are_deterministic() {
        let always = RegimeParams::new(
            (1, 1),
            (1, 1),
            StateTable::uniform(0, DEN_CAP).unwrap(),
            StateTable::uniform(0, DEN_CAP).unwrap(),
        )
        .unwrap();
        let mut proc = RegimeProcess::new(always);
        let mut rng = Prng::new(7);
        assert_eq!(proc.state(), Regime::Calm);
        proc.step(&mut rng);
        assert_eq!(proc.state(), Regime::Storm, "1/1 always transitions");
        proc.step(&mut rng);
        assert_eq!(proc.state(), Regime::Calm);
    }

    /// A calm state at `0/1` never faults; a storm at `1/1` always faults, and
    /// picks from the eligible list.
    #[test]
    fn sample_honors_the_active_table() {
        let params = RegimeParams::new(
            (1, 2),
            (1, 2),
            StateTable::uniform(0, DEN_CAP).unwrap(),
            StateTable::uniform(DEN_CAP, DEN_CAP).unwrap(),
        )
        .unwrap();
        let calm = RegimeProcess::new(params.clone());
        let mut rng = Prng::new(1);
        for _ in 0..50 {
            assert_eq!(
                calm.sample(DecisionClass::BlockIo, &mut rng),
                Answer::Nominal
            );
        }
        // Force into storm and sample: always a block fault.
        let mut storm = RegimeProcess::new(params);
        storm.state = Regime::Storm;
        for _ in 0..50 {
            match storm.sample(DecisionClass::BlockIo, &mut rng) {
                Answer::Fault(f) => assert_eq!(f.class(), DecisionClass::BlockIo),
                other => panic!("storm at 1/1 must fault, got {other:?}"),
            }
        }
    }

    /// A supply class always answers nominally regardless of state.
    #[test]
    fn supply_class_never_faults() {
        let mut proc = RegimeProcess::from_seed(42);
        proc.state = Regime::Storm;
        let mut rng = Prng::new(9);
        for _ in 0..50 {
            assert_eq!(
                proc.sample(DecisionClass::Entropy, &mut rng),
                Answer::Nominal
            );
        }
    }
}
