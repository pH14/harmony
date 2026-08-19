// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deterministic, snapshot-aware delta debugging for replayable step sequences.

use std::{error::Error, fmt, num::NonZeroUsize, thread};

/// Endpoint returned by one deterministic replay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayEndpoint<Snapshot, Outcome> {
    /// Replayable state immediately after the supplied steps.
    pub snapshot: Snapshot,
    /// Outcome inspected by the caller's pass/fail predicate.
    pub outcome: Outcome,
}

/// Game-neutral replay seam used by the reducer.
pub trait SequenceReplay<Step>: Sync {
    /// Replayable state at a segment boundary.
    type Snapshot: Clone + Send + Sync;
    /// Observable endpoint tested by the caller.
    type Outcome: Send;
    /// Deterministic replay failure.
    type Error: Error + Send + Sync + 'static;

    /// Replay `steps` from `entry` and return the exact endpoint.
    fn replay(
        &self,
        entry: &Self::Snapshot,
        steps: &[Step],
    ) -> Result<ReplayEndpoint<Self::Snapshot, Self::Outcome>, Self::Error>;
}

/// Reducer configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReductionConfig {
    /// Maximum deletion candidates replayed concurrently.
    pub workers: NonZeroUsize,
}

/// Reduction accounting for one caller-supplied segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SegmentReduction {
    /// Zero-based segment index.
    pub index: usize,
    /// Steps in the segment before reduction.
    pub original_steps: usize,
    /// Steps in the segment after reduction.
    pub reduced_steps: usize,
    /// Candidate suffixes replayed while reducing this segment.
    pub candidate_replays: u64,
}

/// Successful deterministic reduction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReductionResult<Step> {
    /// Reduced steps in execution order.
    pub steps: Vec<Step>,
    /// Original zero-based index of each surviving step.
    pub original_indices: Vec<usize>,
    /// Per-segment reduction accounting.
    pub segments: Vec<SegmentReduction>,
    /// Total deletion candidates replayed.
    pub candidate_replays: u64,
    /// Non-candidate replays: original check, segment stitching, and final check.
    pub verification_replays: u64,
}

/// Failure to reduce a replayable sequence.
#[derive(Debug)]
pub enum ReductionError<E> {
    /// A replay failed before producing an outcome.
    Replay(E),
    /// The supplied sequence does not satisfy the oracle.
    OriginalDidNotPass,
    /// The final power-on replay rejected the stitched reduction.
    FinalDidNotPass,
    /// A segment's settled sequence did not reach its registered exit.
    SegmentDidNotPass(usize),
    /// A candidate replay thread panicked.
    WorkerPanicked,
}

impl<E: fmt::Display> fmt::Display for ReductionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Replay(error) => write!(formatter, "sequence replay failed: {error}"),
            Self::OriginalDidNotPass => formatter.write_str("the original sequence did not pass"),
            Self::FinalDidNotPass => formatter.write_str("the stitched reduction did not pass"),
            Self::SegmentDidNotPass(index) => {
                write!(formatter, "reduced segment {index} did not reach its exit")
            }
            Self::WorkerPanicked => formatter.write_str("a candidate replay worker panicked"),
        }
    }
}

impl<E: Error + 'static> Error for ReductionError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Replay(error) => Some(error),
            Self::OriginalDidNotPass
            | Self::FinalDidNotPass
            | Self::SegmentDidNotPass(_)
            | Self::WorkerPanicked => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TrackedStep<Step> {
    original_index: usize,
    step: Step,
}

type SegmentResult<Step, ReplayError> =
    Result<(Vec<TrackedStep<Step>>, u64), ReductionError<ReplayError>>;

/// Return a conservative upper bound for ddmin deletion-candidate replays.
///
/// The estimate deliberately assumes that every granularity is exhausted and
/// that successful deletions require another complete sweep. It is intended for
/// projecting replay cost before an expensive reduction, not for campaign state.
#[must_use]
pub fn projected_candidate_replays(segment_lengths: &[usize]) -> u64 {
    segment_lengths.iter().fold(0_u64, |total, &length| {
        if length == 0 {
            return total;
        }
        let levels = usize::BITS - length.saturating_sub(1).leading_zeros();
        let length = u64::try_from(length).unwrap_or(u64::MAX);
        let levels = u64::from(levels).saturating_add(1);
        total.saturating_add(length.saturating_mul(levels).saturating_mul(2))
    })
}

/// Reduce a replayable sequence segment by segment, from left to right.
///
/// `snapshot_points` are step offsets strictly inside the original sequence;
/// unsorted and duplicate points are accepted and normalized. Every candidate
/// deletion is replayed from the current segment's entry snapshot through all
/// later segments, so timing shifts in the retained suffix participate in the
/// oracle. After a segment settles, its new endpoint becomes the next segment's
/// entry snapshot. A final replay from `initial_snapshot` verifies the stitched
/// sequence.
///
/// Candidate batches are evaluated concurrently, but the first passing deletion
/// in deterministic chunk order is always the one retained.
///
/// # Errors
///
/// Returns an error if any replay fails, a worker panics, or either the original
/// or final stitched sequence does not satisfy `passes`.
pub fn reduce_sequence<Replay, Step, Passes>(
    replay: &Replay,
    initial_snapshot: Replay::Snapshot,
    steps: Vec<Step>,
    snapshot_points: &[usize],
    config: ReductionConfig,
    passes: Passes,
) -> Result<ReductionResult<Step>, ReductionError<Replay::Error>>
where
    Replay: SequenceReplay<Step>,
    Step: Clone + Send + Sync,
    Passes: Fn(&Replay::Outcome) -> bool + Sync,
{
    reduce_sequence_inner(
        replay,
        initial_snapshot,
        steps,
        snapshot_points,
        config,
        passes,
        true,
    )
}

/// Reduce a sequence whose original replay the caller has already verified.
///
/// This is the measurement-pass entry point: callers that must measure and
/// print replay speed before a long reduction can reuse that passing replay
/// instead of immediately repeating it. Candidate and final verification
/// behavior is identical to the checked entry point.
///
/// # Errors
///
/// Returns an error if any replay fails, a worker panics, or the final stitched
/// sequence does not satisfy the predicate.
pub fn reduce_verified_sequence<Replay, Step, Passes>(
    replay: &Replay,
    initial_snapshot: Replay::Snapshot,
    steps: Vec<Step>,
    snapshot_points: &[usize],
    config: ReductionConfig,
    passes: Passes,
) -> Result<ReductionResult<Step>, ReductionError<Replay::Error>>
where
    Replay: SequenceReplay<Step>,
    Step: Clone + Send + Sync,
    Passes: Fn(&Replay::Outcome) -> bool + Sync,
{
    reduce_sequence_inner(
        replay,
        initial_snapshot,
        steps,
        snapshot_points,
        config,
        passes,
        false,
    )
}

/// Reduce each segment against its own exit oracle, then verify the stitched sequence.
///
/// Candidate cuts replay only the segment currently being reduced. After a
/// segment settles, its exact endpoint becomes the next segment's entry
/// snapshot. The final replay starts from the original snapshot and applies the
/// caller's end-to-end oracle.
///
/// # Errors
///
/// Returns an error if replay fails, a worker panics, a settled segment misses
/// its exit, or the final stitched replay fails.
pub fn reduce_verified_segmented_sequence<Replay, Step, SegmentPasses, FinalPasses>(
    replay: &Replay,
    initial_snapshot: Replay::Snapshot,
    steps: Vec<Step>,
    snapshot_points: &[usize],
    config: ReductionConfig,
    segment_passes: SegmentPasses,
    final_passes: FinalPasses,
) -> Result<ReductionResult<Step>, ReductionError<Replay::Error>>
where
    Replay: SequenceReplay<Step>,
    Step: Clone + Send + Sync,
    SegmentPasses: Fn(usize, &Replay::Outcome) -> bool + Sync,
    FinalPasses: Fn(&Replay::Outcome) -> bool + Sync,
{
    let mut boundaries = snapshot_points
        .iter()
        .copied()
        .filter(|&point| point > 0 && point < steps.len())
        .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries.insert(0, 0);
    boundaries.push(steps.len());

    let tracked = steps
        .into_iter()
        .enumerate()
        .map(|(original_index, step)| TrackedStep {
            original_index,
            step,
        })
        .collect::<Vec<_>>();
    let mut segments = boundaries
        .windows(2)
        .map(|window| tracked[window[0]..window[1]].to_vec())
        .collect::<Vec<_>>();
    let mut entry = initial_snapshot.clone();
    let mut reports = Vec::with_capacity(segments.len());
    let mut candidate_replays = 0_u64;
    let mut verification_replays = 0_u64;

    for (segment_index, segment) in segments.iter_mut().enumerate() {
        let original_steps = segment.len();
        let local_passes = |outcome: &Replay::Outcome| segment_passes(segment_index, outcome);
        let (reduced, replays) = reduce_segment(
            replay,
            &entry,
            segment.clone(),
            &[],
            config.workers,
            &local_passes,
        )?;
        candidate_replays = candidate_replays.saturating_add(replays);
        *segment = reduced;

        let segment_steps = segment
            .iter()
            .map(|tracked| tracked.step.clone())
            .collect::<Vec<_>>();
        let endpoint = replay
            .replay(&entry, &segment_steps)
            .map_err(ReductionError::Replay)?;
        verification_replays = verification_replays.saturating_add(1);
        if !segment_passes(segment_index, &endpoint.outcome) {
            return Err(ReductionError::SegmentDidNotPass(segment_index));
        }
        entry = endpoint.snapshot;
        reports.push(SegmentReduction {
            index: segment_index,
            original_steps,
            reduced_steps: segment.len(),
            candidate_replays: replays,
        });
    }

    let stitched = flatten_steps(&segments);
    let final_endpoint = replay
        .replay(&initial_snapshot, &stitched)
        .map_err(ReductionError::Replay)?;
    verification_replays = verification_replays.saturating_add(1);
    if !final_passes(&final_endpoint.outcome) {
        return Err(ReductionError::FinalDidNotPass);
    }

    let original_indices = segments
        .iter()
        .flatten()
        .map(|tracked| tracked.original_index)
        .collect();
    Ok(ReductionResult {
        steps: stitched,
        original_indices,
        segments: reports,
        candidate_replays,
        verification_replays,
    })
}

fn reduce_sequence_inner<Replay, Step, Passes>(
    replay: &Replay,
    initial_snapshot: Replay::Snapshot,
    steps: Vec<Step>,
    snapshot_points: &[usize],
    config: ReductionConfig,
    passes: Passes,
    verify_original: bool,
) -> Result<ReductionResult<Step>, ReductionError<Replay::Error>>
where
    Replay: SequenceReplay<Step>,
    Step: Clone + Send + Sync,
    Passes: Fn(&Replay::Outcome) -> bool + Sync,
{
    let mut boundaries = snapshot_points
        .iter()
        .copied()
        .filter(|&point| point > 0 && point < steps.len())
        .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries.insert(0, 0);
    boundaries.push(steps.len());

    let tracked = steps
        .into_iter()
        .enumerate()
        .map(|(original_index, step)| TrackedStep {
            original_index,
            step,
        })
        .collect::<Vec<_>>();
    let mut segments = boundaries
        .windows(2)
        .map(|window| tracked[window[0]..window[1]].to_vec())
        .collect::<Vec<_>>();

    if verify_original {
        let original = flatten_steps(&segments);
        let original_endpoint = replay
            .replay(&initial_snapshot, &original)
            .map_err(ReductionError::Replay)?;
        if !passes(&original_endpoint.outcome) {
            return Err(ReductionError::OriginalDidNotPass);
        }
    }

    let mut entry = initial_snapshot.clone();
    let mut reports = Vec::with_capacity(segments.len());
    let mut candidate_replays = 0_u64;
    let mut verification_replays = u64::from(verify_original);
    for segment_index in 0..segments.len() {
        let original_steps = segments[segment_index].len();
        let later = flatten_tracked(&segments[segment_index + 1..]);
        let (reduced, replays) = reduce_segment(
            replay,
            &entry,
            segments[segment_index].clone(),
            &later,
            config.workers,
            &passes,
        )?;
        candidate_replays = candidate_replays.saturating_add(replays);
        segments[segment_index] = reduced;

        let segment_steps = segments[segment_index]
            .iter()
            .map(|tracked| tracked.step.clone())
            .collect::<Vec<_>>();
        let endpoint = replay
            .replay(&entry, &segment_steps)
            .map_err(ReductionError::Replay)?;
        verification_replays = verification_replays.saturating_add(1);
        entry = endpoint.snapshot;
        reports.push(SegmentReduction {
            index: segment_index,
            original_steps,
            reduced_steps: segments[segment_index].len(),
            candidate_replays: replays,
        });
    }

    let stitched = flatten_steps(&segments);
    let final_endpoint = replay
        .replay(&initial_snapshot, &stitched)
        .map_err(ReductionError::Replay)?;
    verification_replays = verification_replays.saturating_add(1);
    if !passes(&final_endpoint.outcome) {
        return Err(ReductionError::FinalDidNotPass);
    }

    let original_indices = segments
        .iter()
        .flatten()
        .map(|tracked| tracked.original_index)
        .collect();
    Ok(ReductionResult {
        steps: stitched,
        original_indices,
        segments: reports,
        candidate_replays,
        verification_replays,
    })
}

fn reduce_segment<Replay, Step, Passes>(
    replay: &Replay,
    entry: &Replay::Snapshot,
    mut segment: Vec<TrackedStep<Step>>,
    later: &[TrackedStep<Step>],
    workers: NonZeroUsize,
    passes: &Passes,
) -> SegmentResult<Step, Replay::Error>
where
    Replay: SequenceReplay<Step>,
    Step: Clone + Send + Sync,
    Passes: Fn(&Replay::Outcome) -> bool + Sync,
{
    let mut granularity = workers.get().min(segment.len()).max(2);
    let mut replay_count = 0_u64;
    while !segment.is_empty() {
        granularity = granularity.min(segment.len());
        let mut candidates = Vec::with_capacity(granularity);
        for part in 0..granularity {
            let start = part.saturating_mul(segment.len()) / granularity;
            let end = (part.saturating_add(1)).saturating_mul(segment.len()) / granularity;
            let mut candidate = Vec::with_capacity(
                segment
                    .len()
                    .saturating_sub(end.saturating_sub(start))
                    .saturating_add(later.len()),
            );
            candidate.extend_from_slice(&segment[..start]);
            candidate.extend_from_slice(&segment[end..]);
            candidate.extend_from_slice(later);
            candidates.push(candidate);
        }
        let (accepted, evaluated) =
            evaluate_candidates(replay, entry, &candidates, workers, passes)?;
        replay_count = replay_count.saturating_add(evaluated);
        if let Some(index) = accepted {
            let start = index.saturating_mul(segment.len()) / granularity;
            let end = (index.saturating_add(1)).saturating_mul(segment.len()) / granularity;
            segment.drain(start..end);
            granularity = granularity.saturating_sub(1).max(2);
        } else if granularity == segment.len() {
            break;
        } else {
            granularity = granularity.saturating_mul(2).min(segment.len());
        }
    }
    Ok((segment, replay_count))
}

fn evaluate_candidates<Replay, Step, Passes>(
    replay: &Replay,
    entry: &Replay::Snapshot,
    candidates: &[Vec<TrackedStep<Step>>],
    workers: NonZeroUsize,
    passes: &Passes,
) -> Result<(Option<usize>, u64), ReductionError<Replay::Error>>
where
    Replay: SequenceReplay<Step>,
    Step: Clone + Send + Sync,
    Passes: Fn(&Replay::Outcome) -> bool + Sync,
{
    let mut offset = 0_usize;
    for wave in candidates.chunks(workers.get()) {
        let wave_results = thread::scope(|scope| {
            let handles = wave
                .iter()
                .map(|candidate| {
                    scope.spawn(move || {
                        let steps = candidate
                            .iter()
                            .map(|tracked| tracked.step.clone())
                            .collect::<Vec<_>>();
                        replay
                            .replay(entry, &steps)
                            .map(|endpoint| passes(&endpoint.outcome))
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join())
                .collect::<Vec<_>>()
        });
        let mut accepted = Vec::with_capacity(wave.len());
        for result in wave_results {
            match result {
                Ok(Ok(passed)) => accepted.push(passed),
                Ok(Err(error)) => return Err(ReductionError::Replay(error)),
                Err(_) => return Err(ReductionError::WorkerPanicked),
            }
        }
        if let Some(index) = accepted.iter().position(|&passed| passed) {
            let evaluated = offset.saturating_add(wave.len());
            return Ok((
                Some(offset.saturating_add(index)),
                u64::try_from(evaluated).unwrap_or(u64::MAX),
            ));
        }
        offset = offset.saturating_add(wave.len());
    }
    Ok((None, u64::try_from(candidates.len()).unwrap_or(u64::MAX)))
}

fn flatten_steps<Step: Clone>(segments: &[Vec<TrackedStep<Step>>]) -> Vec<Step> {
    segments
        .iter()
        .flatten()
        .map(|tracked| tracked.step.clone())
        .collect()
}

fn flatten_tracked<Step: Clone>(segments: &[Vec<TrackedStep<Step>>]) -> Vec<TrackedStep<Step>> {
    segments.iter().flatten().cloned().collect()
}

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        num::NonZeroUsize,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::{
        ReductionConfig, ReductionError, ReplayEndpoint, SequenceReplay, TrackedStep,
        evaluate_candidates, projected_candidate_replays, reduce_sequence,
        reduce_verified_segmented_sequence,
    };

    struct SumReplay;

    impl SequenceReplay<u8> for SumReplay {
        type Snapshot = u64;
        type Outcome = u64;
        type Error = Infallible;

        fn replay(
            &self,
            entry: &Self::Snapshot,
            steps: &[u8],
        ) -> Result<ReplayEndpoint<Self::Snapshot, Self::Outcome>, Self::Error> {
            let outcome = steps
                .iter()
                .fold(*entry, |sum, step| sum.saturating_add(u64::from(*step)));
            Ok(ReplayEndpoint {
                snapshot: outcome,
                outcome,
            })
        }
    }

    fn config() -> ReductionConfig {
        ReductionConfig {
            workers: NonZeroUsize::new(4).expect("four is nonzero"),
        }
    }

    #[test]
    fn reduces_left_to_right_and_preserves_original_indices() {
        let reduced = reduce_sequence(
            &SumReplay,
            0,
            vec![0, 4, 0, 0, 6, 0],
            &[3],
            config(),
            |outcome| *outcome >= 10,
        )
        .expect("sum sequence reduces");
        assert_eq!(reduced.steps, vec![4, 6]);
        assert_eq!(reduced.original_indices, vec![1, 4]);
        assert_eq!(reduced.segments.len(), 2);
        assert_eq!(reduced.segments[0].reduced_steps, 1);
        assert_eq!(reduced.segments[1].reduced_steps, 1);
    }

    #[test]
    fn timing_shift_in_later_segments_participates_in_every_candidate() {
        struct PositionReplay;
        impl SequenceReplay<i8> for PositionReplay {
            type Snapshot = (i64, usize);
            type Outcome = (i64, usize);
            type Error = Infallible;

            fn replay(
                &self,
                entry: &Self::Snapshot,
                steps: &[i8],
            ) -> Result<ReplayEndpoint<Self::Snapshot, Self::Outcome>, Self::Error> {
                let mut state = *entry;
                for step in steps {
                    state.0 = state.0.saturating_add(i64::from(*step));
                    state.1 = state.1.saturating_add(1);
                }
                Ok(ReplayEndpoint {
                    snapshot: state,
                    outcome: state,
                })
            }
        }

        let reduced = reduce_sequence(
            &PositionReplay,
            (0, 0),
            vec![1, 1, 1, 1],
            &[2],
            config(),
            |outcome| *outcome == (4, 4),
        )
        .expect("position-sensitive sequence remains whole");
        assert_eq!(reduced.steps, vec![1, 1, 1, 1]);
    }

    #[test]
    fn rejects_an_original_that_does_not_pass() {
        let error = reduce_sequence(&SumReplay, 0, vec![1], &[], config(), |outcome| {
            *outcome > 1
        })
        .expect_err("the original must pass");
        assert!(matches!(error, ReductionError::OriginalDidNotPass));
    }

    #[test]
    fn projection_is_zero_only_for_empty_segments() {
        assert_eq!(projected_candidate_replays(&[0, 0]), 0);
        assert!(projected_candidate_replays(&[1, 8, 64]) > 0);
    }

    #[test]
    fn candidate_evaluation_stops_after_the_first_passing_wave() {
        let candidates = (0..5)
            .map(|value| {
                vec![TrackedStep {
                    original_index: value,
                    step: u8::try_from(value).expect("small candidate"),
                }]
            })
            .collect::<Vec<_>>();
        let workers = NonZeroUsize::new(2).expect("two is nonzero");
        let (accepted, evaluated) =
            evaluate_candidates(&SumReplay, &0, &candidates, workers, &|outcome| {
                *outcome == 0
            })
            .expect("candidate evaluation");
        assert_eq!(accepted, Some(0));
        assert_eq!(evaluated, 2);
    }

    #[test]
    fn segment_local_candidates_do_not_replay_later_segments() {
        struct BoundedReplay {
            longest: AtomicUsize,
        }
        impl SequenceReplay<u8> for BoundedReplay {
            type Snapshot = u64;
            type Outcome = u64;
            type Error = Infallible;

            fn replay(
                &self,
                entry: &Self::Snapshot,
                steps: &[u8],
            ) -> Result<ReplayEndpoint<Self::Snapshot, Self::Outcome>, Self::Error> {
                self.longest.fetch_max(steps.len(), Ordering::SeqCst);
                let outcome = steps
                    .iter()
                    .fold(*entry, |sum, step| sum.saturating_add(u64::from(*step)));
                Ok(ReplayEndpoint {
                    snapshot: outcome,
                    outcome,
                })
            }
        }

        let replay = BoundedReplay {
            longest: AtomicUsize::new(0),
        };
        let reduced = reduce_verified_segmented_sequence(
            &replay,
            0,
            vec![0, 4, 0, 0, 6, 0],
            &[3],
            config(),
            |index, outcome| *outcome >= if index == 0 { 4 } else { 10 },
            |outcome| *outcome >= 10,
        )
        .expect("segment-local reduction");
        assert_eq!(reduced.steps, vec![4, 6]);
        assert!(
            replay.longest.load(Ordering::SeqCst) <= 3,
            "candidate replay crossed a segment boundary"
        );
    }
}
