// SPDX-License-Identifier: AGPL-3.0-or-later
//! Property tests for [`FilmPlan`] derivation (task 87 gate 1): synthetic frame
//! clocks with gaps, chunked windows, and stride edge cases. Pure logic — no
//! ROM, no core, no session.

use film::{BillboardWindow, ClipSelect, FilmPlan, FrameTick, HEADER_LEN, PlanError};
use proptest::prelude::*;

/// A strictly-increasing frame clock: `1..=64` ticks whose `frame` and `moment`
/// both advance by a positive delta each step (frame gaps allowed — the deltas
/// are ≥ 1, not exactly 1).
fn valid_clock() -> impl Strategy<Value = Vec<FrameTick>> {
    prop::collection::vec((1u32..=8, 1u64..=10_000), 1..=64).prop_map(|deltas| {
        let mut frame = 0u32;
        let mut moment = 0u64;
        deltas
            .into_iter()
            .map(|(df, dm)| {
                frame += df;
                moment += dm;
                FrameTick { frame, moment }
            })
            .collect()
    })
}

/// A billboard window big enough to hold a header, at an arbitrary base that
/// cannot overflow.
fn window() -> impl Strategy<Value = BillboardWindow> {
    (0u64..0xFFFF_0000, (HEADER_LEN as u32)..=(64 * 1024))
        .prop_map(|(gpa, len)| BillboardWindow { gpa, len })
}

/// The case count: the ≥256 the conventions require, dropped under Miri (10–100×
/// slower interpreted) so the unsafe-free crate's Miri run stays quick.
const CASES: u32 = if cfg!(miri) { 16 } else { 256 };

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    /// `All` + stride 1 films exactly the clock, in order, and the read chunks
    /// reassemble to the whole window (each ≤ the cap, contiguous, ascending).
    #[test]
    fn all_selects_the_whole_clock_and_chunks_reassemble(
        clock in valid_clock(),
        win in window(),
        cap in 1u32..=(64 * 1024),
    ) {
        let plan = FilmPlan::derive(&clock, win, ClipSelect::All, None, cap).unwrap();
        prop_assert_eq!(plan.frames.len(), clock.len());
        for (shot, tick) in plan.frames.iter().zip(&clock) {
            prop_assert_eq!(shot.frame, tick.frame);
            prop_assert_eq!(shot.moment, tick.moment);
        }
        let chunks = plan.read_chunks();
        prop_assert!(!chunks.is_empty());
        prop_assert_eq!(chunks.iter().map(|c| c.len as u64).sum::<u64>(), u64::from(win.len));
        prop_assert_eq!(chunks[0].gpa, win.gpa);
        for c in &chunks {
            prop_assert!(c.len <= cap);
            prop_assert!(c.len > 0);
        }
        for w in chunks.windows(2) {
            prop_assert_eq!(w[0].gpa + u64::from(w[0].len), w[1].gpa);
        }
    }

    /// Stride thins the clip to every Nth frame; the count is the exact ceiling
    /// and the kept frames are the strided subset.
    #[test]
    fn stride_keeps_every_nth_frame(
        clock in valid_clock(),
        win in window(),
        stride in 1u32..=10,
    ) {
        let plan = FilmPlan::derive(&clock, win, ClipSelect::All, Some(stride), 1 << 16).unwrap();
        let expected: Vec<u32> = clock
            .iter()
            .step_by(stride as usize)
            .map(|t| t.frame)
            .collect();
        let got: Vec<u32> = plan.frames.iter().map(|s| s.frame).collect();
        prop_assert_eq!(got, expected);
        prop_assert_eq!(plan.stride, stride);
    }

    /// A moment-span clip selects exactly the ticks whose moment lies in the span
    /// (and nothing outside it).
    #[test]
    fn moment_span_selects_the_inclusive_range(
        clock in valid_clock(),
        win in window(),
        a in 0u64..700_000,
        b in 0u64..700_000,
    ) {
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let result = FilmPlan::derive(
            &clock,
            win,
            ClipSelect::MomentSpan { start, end },
            None,
            1 << 16,
        );
        let expected: Vec<u64> = clock
            .iter()
            .filter(|t| t.moment >= start && t.moment <= end)
            .map(|t| t.moment)
            .collect();
        match result {
            Ok(plan) => {
                let got: Vec<u64> = plan.frames.iter().map(|s| s.moment).collect();
                prop_assert_eq!(got, expected);
            }
            Err(PlanError::EmptyClip) => {
                prop_assert!(expected.is_empty());
            }
            Err(e) => prop_assert!(false, "unexpected error {e:?}"),
        }
    }

    /// Derivation is total: arbitrary (even malformed) inputs yield `Ok`/`Err`,
    /// never a panic.
    #[test]
    fn derive_never_panics_on_arbitrary_input(
        raw in prop::collection::vec((any::<u32>(), any::<u64>()), 0..64),
        gpa in any::<u64>(),
        len in any::<u32>(),
        stride in prop::option::of(any::<u32>()),
        cap in any::<u32>(),
        pick in 0u8..3,
    ) {
        let ticks: Vec<FrameTick> = raw
            .into_iter()
            .map(|(frame, moment)| FrameTick { frame, moment })
            .collect();
        let clip = match pick {
            0 => ClipSelect::All,
            1 => ClipSelect::MomentSpan { start: 0, end: u64::MAX },
            _ => ClipSelect::FrameRange { first: 0, last: u32::MAX },
        };
        let _ = FilmPlan::derive(&ticks, BillboardWindow { gpa, len }, clip, stride, cap);
    }
}
