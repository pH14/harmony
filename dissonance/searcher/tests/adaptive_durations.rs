// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::BTreeSet,
    num::{NonZeroU64, NonZeroUsize},
};

use searcher::search::{
    duration::{DurationCheckpoint, DurationPolicy},
    rand::RomuDuoJrRand,
};
use serde::{Deserialize, Serialize};

const MAX_DURATION: u64 = 64;
const FAST_WORK_BUDGET: u64 = 16_384;
const DELAYED_WORK_BUDGET: u64 = 4_096;
const PHASE_WARM_OBSERVATIONS: usize = 192;
const PHASE_TAIL_WORK_BUDGET: u64 = 8_192;
const SEEDS: [u64; 8] = [
    0x5eed_0001,
    0x5eed_0002,
    0x5eed_0003,
    0x5eed_0004,
    0x5eed_0005,
    0x5eed_0006,
    0x5eed_0007,
    0x5eed_0008,
];

fn positive(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("positive value")
}

fn duration_scales(max_duration: u64) -> Vec<u64> {
    let mut scales = Vec::new();
    let mut duration = 1;
    while duration <= max_duration {
        scales.push(duration);
        duration = match duration.checked_mul(2) {
            Some(next) => next,
            None => break,
        };
    }
    scales
}

fn observe(policy: &mut DurationPolicy, duration: u64, useful: bool, execution_cost: u64) {
    policy
        .observe(positive(duration), useful, positive(execution_cost))
        .expect("valid duration observation");
}

fn uniform_duration(rand: &mut RomuDuoJrRand, max_duration: u64) -> u64 {
    let scale_count = duration_scales(max_duration).len();
    let bound = NonZeroUsize::new(scale_count).expect("at least one duration scale");
    1_u64 << rand.below(bound)
}

#[derive(Default)]
struct FastTransition {
    reached: bool,
}

impl FastTransition {
    fn step(&mut self, duration: u64) -> bool {
        if self.reached {
            return false;
        }
        if duration == 1 {
            self.reached = true;
            true
        } else {
            false
        }
    }
}

#[derive(Default)]
struct DelayedTransition {
    reached: bool,
}

impl DelayedTransition {
    fn step(&mut self, duration: u64) -> bool {
        if self.reached {
            return false;
        }
        if duration >= 32 {
            self.reached = true;
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Fast,
    Delayed,
}

impl Phase {
    fn accepts(self, duration: u64) -> bool {
        match self {
            Self::Fast => duration == 1,
            Self::Delayed => duration >= 32,
        }
    }
}

struct PhaseTransition {
    phase: Phase,
    reached: bool,
}

impl PhaseTransition {
    fn new(phase: Phase) -> Self {
        Self {
            phase,
            reached: false,
        }
    }

    fn set_phase(&mut self, phase: Phase) {
        self.phase = phase;
        self.reached = false;
    }

    fn step(&mut self, duration: u64) -> bool {
        if self.reached {
            return false;
        }
        if self.phase.accepts(duration) {
            self.reached = true;
            true
        } else {
            false
        }
    }

    fn reset(&mut self) {
        self.reached = false;
    }
}

#[derive(Default)]
struct Score {
    successes: u64,
    logical_work: u64,
}

fn run_fast_adaptive(seed: u64) -> Score {
    let mut policy = DurationPolicy::new();
    let mut rand = RomuDuoJrRand::with_seed(seed);
    let mut state = FastTransition::default();
    let mut score = Score::default();
    while score.logical_work < FAST_WORK_BUDGET {
        let duration = policy.draw(&mut rand, positive(MAX_DURATION));
        let useful = state.step(duration);
        score.logical_work += duration;
        observe(&mut policy, duration, useful, duration);
        if useful {
            score.successes += 1;
            state = FastTransition::default();
        }
    }
    score
}

fn run_fast_uniform(seed: u64) -> Score {
    let mut rand = RomuDuoJrRand::with_seed(seed);
    let mut state = FastTransition::default();
    let mut score = Score::default();
    while score.logical_work < FAST_WORK_BUDGET {
        let duration = uniform_duration(&mut rand, MAX_DURATION);
        let useful = state.step(duration);
        score.logical_work += duration;
        if useful {
            score.successes += 1;
            state = FastTransition::default();
        }
    }
    score
}

fn train_short_preference(policy: &mut DurationPolicy) {
    for _ in 0..128 {
        observe(policy, 1, true, 1);
    }
}

fn run_delayed_after_short_training(seed: u64) -> (Score, u64) {
    let mut policy = DurationPolicy::new();
    train_short_preference(&mut policy);
    let mut rand = RomuDuoJrRand::with_seed(seed);
    let mut state = DelayedTransition::default();
    let mut score = Score::default();
    let mut unproductive_steps = 0;
    while !state.reached && score.logical_work < DELAYED_WORK_BUDGET {
        let duration = policy.draw(&mut rand, positive(MAX_DURATION));
        let useful = state.step(duration);
        score.logical_work += duration;
        observe(&mut policy, duration, useful, duration);
        if useful {
            score.successes += 1;
        } else {
            unproductive_steps += 1;
        }
    }
    (score, unproductive_steps)
}

fn run_phase_observations(
    policy: &mut DurationPolicy,
    rand: &mut RomuDuoJrRand,
    state: &mut PhaseTransition,
    phase: Phase,
    observations: usize,
) -> Score {
    state.set_phase(phase);
    let mut score = Score::default();
    for _ in 0..observations {
        let duration = policy.draw(rand, positive(MAX_DURATION));
        let useful = state.step(duration);
        score.logical_work += duration;
        observe(policy, duration, useful, duration);
        if useful {
            score.successes += 1;
            state.reset();
        }
    }
    score
}

fn run_phase_work(policy: &mut DurationPolicy, seed: u64, phase: Phase, adapt: bool) -> Score {
    let mut rand = RomuDuoJrRand::with_seed(seed);
    let mut state = PhaseTransition::new(phase);
    let mut score = Score::default();
    while score.logical_work < PHASE_TAIL_WORK_BUDGET {
        let duration = policy.draw(&mut rand, positive(MAX_DURATION));
        let useful = state.step(duration);
        score.logical_work += duration;
        if adapt {
            observe(policy, duration, useful, duration);
        }
        if useful {
            score.successes += 1;
            state.reset();
        }
    }
    score
}

fn assert_checkpoint_contract<T>()
where
    T: Clone + std::fmt::Debug + Eq + Serialize + for<'de> Deserialize<'de>,
{
}

#[test]
fn draws_are_bounded_logarithmic_and_keep_exploring_after_short_successes() {
    let max_duration = positive(MAX_DURATION);
    let expected: BTreeSet<u64> = duration_scales(MAX_DURATION).into_iter().collect();
    for seed in SEEDS {
        let mut policy = DurationPolicy::new();
        train_short_preference(&mut policy);
        let mut rand = RomuDuoJrRand::with_seed(seed);
        let mut observed = BTreeSet::new();
        for _ in 0..4_096 {
            let duration = policy.draw(&mut rand, max_duration);
            assert!(duration <= MAX_DURATION);
            assert!(duration.is_power_of_two());
            observed.insert(duration);
        }
        assert_eq!(observed, expected);
    }
    for max_duration in [1, 3, 5, 40, 63, 64] {
        let expected: BTreeSet<u64> = duration_scales(max_duration).into_iter().collect();
        let mut rand = RomuDuoJrRand::with_seed(max_duration);
        let policy = DurationPolicy::new();
        for _ in 0..512 {
            let duration = policy.draw(&mut rand, positive(max_duration));
            assert!(expected.contains(&duration));
        }
    }
}

#[test]
fn checkpoint_restore_preserves_state_and_future_draws() {
    assert_checkpoint_contract::<DurationCheckpoint>();
    let max_duration = positive(MAX_DURATION);
    let mut policy = DurationPolicy::new();
    let mut training_rand = RomuDuoJrRand::with_seed(0x51a7_e001);
    for index in 0..256 {
        let duration = policy.draw(&mut training_rand, max_duration);
        observe(
            &mut policy,
            duration,
            duration == 1 || index % 11 == 0,
            duration,
        );
    }
    let checkpoint = policy.checkpoint();
    let encoded = serde_json::to_vec(&checkpoint).expect("serialize checkpoint");
    let decoded: DurationCheckpoint =
        serde_json::from_slice(&encoded).expect("deserialize checkpoint");
    assert_eq!(checkpoint, decoded);
    let mut restored = DurationPolicy::from_checkpoint(decoded).expect("restore checkpoint");
    assert_eq!(policy.checkpoint(), restored.checkpoint());

    let mut policy_rand = RomuDuoJrRand::with_seed(0x51a7_e002);
    let mut restored_rand = RomuDuoJrRand::with_seed(0x51a7_e002);
    for index in 0..2_048 {
        let policy_duration = policy.draw(&mut policy_rand, max_duration);
        let restored_duration = restored.draw(&mut restored_rand, max_duration);
        assert_eq!(policy_duration, restored_duration);
        let useful = policy_duration == 1 || index % 17 == 0;
        observe(&mut policy, policy_duration, useful, policy_duration);
        observe(&mut restored, restored_duration, useful, restored_duration);
        assert_eq!(policy.checkpoint(), restored.checkpoint());
    }
}

#[test]
fn adaptive_policy_improves_fast_transition_success_per_logical_work() {
    let mut adaptive = Score::default();
    let mut uniform = Score::default();
    for seed in SEEDS {
        let score = run_fast_adaptive(seed);
        adaptive.successes += score.successes;
        adaptive.logical_work += score.logical_work;
        let score = run_fast_uniform(seed);
        uniform.successes += score.successes;
        uniform.logical_work += score.logical_work;
    }
    assert!(uniform.successes > 0);
    assert!(adaptive.successes > uniform.successes);
    assert!(adaptive.successes * uniform.logical_work > uniform.successes * adaptive.logical_work);
}

#[test]
fn delayed_transition_is_reached_after_learning_the_fast_interval() {
    let mut total_successes = 0;
    let mut total_work = 0;
    let mut total_unproductive_steps = 0;
    for seed in SEEDS {
        let (score, unproductive_steps) = run_delayed_after_short_training(seed);
        assert_eq!(score.successes, 1);
        assert!(score.logical_work <= DELAYED_WORK_BUDGET + MAX_DURATION);
        total_successes += score.successes;
        total_work += score.logical_work;
        total_unproductive_steps += unproductive_steps;
    }
    assert_eq!(total_successes, SEEDS.len() as u64);
    assert!(total_work > total_successes * 32);
    assert!(total_unproductive_steps > 0);
}

#[test]
fn changing_phase_shifts_late_preference_under_equal_logical_work() {
    let mut delayed_adaptive = Score::default();
    let mut delayed_frozen = Score::default();
    let mut fast_adaptive = Score::default();
    let mut fast_frozen = Score::default();
    for seed in SEEDS {
        let mut adaptive = DurationPolicy::new();
        let mut state = PhaseTransition::new(Phase::Fast);
        let mut warm_rand = RomuDuoJrRand::with_seed(seed ^ 0x1000_0000);
        let fast_warm = run_phase_observations(
            &mut adaptive,
            &mut warm_rand,
            &mut state,
            Phase::Fast,
            PHASE_WARM_OBSERVATIONS,
        );
        assert!(fast_warm.successes > 0);
        let short_checkpoint = adaptive.checkpoint();
        let frozen_short =
            DurationPolicy::from_checkpoint(short_checkpoint).expect("short checkpoint");

        let delayed_warm = run_phase_observations(
            &mut adaptive,
            &mut warm_rand,
            &mut state,
            Phase::Delayed,
            PHASE_WARM_OBSERVATIONS,
        );
        assert!(delayed_warm.successes > 0);
        let long_checkpoint = adaptive.checkpoint();
        let mut adaptive_delayed =
            DurationPolicy::from_checkpoint(long_checkpoint.clone()).expect("long checkpoint");
        let delayed_adaptive_score = run_phase_work(
            &mut adaptive_delayed,
            seed ^ 0x2000_0000,
            Phase::Delayed,
            true,
        );
        let mut frozen_short = frozen_short;
        let delayed_frozen_score =
            run_phase_work(&mut frozen_short, seed ^ 0x2000_0000, Phase::Delayed, false);
        assert!(delayed_adaptive_score.logical_work >= PHASE_TAIL_WORK_BUDGET);
        assert!(delayed_frozen_score.logical_work >= PHASE_TAIL_WORK_BUDGET);
        assert!(delayed_adaptive_score.logical_work <= PHASE_TAIL_WORK_BUDGET + MAX_DURATION);
        assert!(delayed_frozen_score.logical_work <= PHASE_TAIL_WORK_BUDGET + MAX_DURATION);
        delayed_adaptive.successes += delayed_adaptive_score.successes;
        delayed_adaptive.logical_work += delayed_adaptive_score.logical_work;
        delayed_frozen.successes += delayed_frozen_score.successes;
        delayed_frozen.logical_work += delayed_frozen_score.logical_work;

        let mut adaptive_fast =
            DurationPolicy::from_checkpoint(long_checkpoint.clone()).expect("long checkpoint");
        let fast_warm = run_phase_observations(
            &mut adaptive_fast,
            &mut warm_rand,
            &mut state,
            Phase::Fast,
            PHASE_WARM_OBSERVATIONS,
        );
        assert!(fast_warm.successes > 0);
        let mut frozen_long =
            DurationPolicy::from_checkpoint(long_checkpoint).expect("long checkpoint");
        let fast_adaptive_score =
            run_phase_work(&mut adaptive_fast, seed ^ 0x3000_0000, Phase::Fast, true);
        let fast_frozen_score =
            run_phase_work(&mut frozen_long, seed ^ 0x3000_0000, Phase::Fast, false);
        assert!(fast_adaptive_score.logical_work >= PHASE_TAIL_WORK_BUDGET);
        assert!(fast_frozen_score.logical_work >= PHASE_TAIL_WORK_BUDGET);
        assert!(fast_adaptive_score.logical_work <= PHASE_TAIL_WORK_BUDGET + MAX_DURATION);
        assert!(fast_frozen_score.logical_work <= PHASE_TAIL_WORK_BUDGET + MAX_DURATION);
        fast_adaptive.successes += fast_adaptive_score.successes;
        fast_adaptive.logical_work += fast_adaptive_score.logical_work;
        fast_frozen.successes += fast_frozen_score.successes;
        fast_frozen.logical_work += fast_frozen_score.logical_work;
    }
    assert!(delayed_adaptive.successes > delayed_frozen.successes);
    assert!(
        delayed_adaptive.successes * delayed_frozen.logical_work
            > delayed_frozen.successes * delayed_adaptive.logical_work
    );
    assert!(fast_adaptive.successes > fast_frozen.successes);
    assert!(
        fast_adaptive.successes * fast_frozen.logical_work
            > fast_frozen.successes * fast_adaptive.logical_work
    );
}
