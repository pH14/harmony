// SPDX-License-Identifier: AGPL-3.0-or-later
//! The deterministic **maze** benchmark workload (task 134, `hm-cs5`): a
//! pure-integer gauntlet grid whose walk is a function of the caller-supplied
//! entropy stream alone — the fault-free, game-shaped workload the first
//! cooperative Differential exploration gate measures archive-guided search
//! on. The crate holds only the workload logic (the grid, the transition
//! function, and the exact reachability/plateau accounting the gate's
//! non-vacuity claim is checked against); emission over the SDK wire, cells,
//! and campaign policy live with their owners (`campaign-runner` host-side,
//! the maze guest agent under `harmony-linux/`), which share this one
//! transition function so the portable machine and the guest cannot drift.
//!
//! ## The shape (task 84's recommended maze, concretely)
//!
//! A [`MazeSpec`] names `levels` stacked corridors of `width` tiles. The
//! walker starts at `(x = 0, level = 0)` and consumes one entropy byte per
//! [`step`]: inside a corridor the byte drives a rightward-drifting random
//! walk; at the corridor's end tile the byte picks one of `doors` doors. One
//! door per level is correct (a pure function of the [`MazeSpec::maze_seed`])
//! and advances the walker to the next level's start; every other door
//! **resets the walk to the maze start** `(0, 0)` (task 84's junction-reset
//! shape). Completing the last level reaches the **goal** (absorbing).
//!
//! This makes random search geometrically poor in depth — from the start,
//! reaching level `d` needs `d` consecutive correct draws (each `1/doors`),
//! and every wrong draw sends the walk back to re-climb the whole gauntlet —
//! while a search that returns exactly to a retained deep state re-draws only
//! the next door: roughly linear in depth. Precisely the property (the
//! Metroid discipline) that makes the exploration gate non-vacuous. The
//! reachable-cell count is exact ([`reachable_cells`]) so the gate can check
//! the claim, not just state it.
//!
//! Determinism discipline (conventions rule 4): everything is integer state;
//! the walk is a pure function `(spec, state, byte) → state`; the crate draws
//! no entropy of its own and never panics on any input.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// The maze's manifest parameters — the workload's shape, fixed for a whole
/// campaign and recorded verbatim in the gate report. The maze itself (which
/// door is correct at each level) is a pure function of these values; the
/// campaign seed varies only the walker's entropy, never the maze.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MazeSpec {
    /// Corridor length per level, in tiles (≥ 2: a start tile and a junction
    /// tile). The junction sits at `x = width - 1`.
    pub width: u32,
    /// Stacked corridor levels between the start and the goal (≥ 1).
    /// Completing level `levels - 1` reaches the goal.
    pub levels: u32,
    /// Doors at each junction (≥ 2). Exactly one per level is correct.
    pub doors: u32,
    /// Fixes which door is correct at each level. Part of the workload
    /// manifest — *not* the campaign seed.
    pub maze_seed: u64,
}

impl MazeSpec {
    /// A small default gauntlet: deep enough that random restart plateaus
    /// well short of the goal at portable budgets, small enough that an
    /// archive-guided campaign completes it in tens of branches.
    pub fn small() -> Self {
        MazeSpec {
            width: 4,
            levels: 6,
            doors: 4,
            maze_seed: 0x6d61_7a65, // "maze"
        }
    }

    /// The clamped corridor width (the transition function's own floor; a
    /// degenerate spec is walked as its clamped form rather than panicking).
    pub fn width(&self) -> u32 {
        self.width.max(2)
    }

    /// The clamped level count.
    pub fn levels(&self) -> u32 {
        self.levels.max(1)
    }

    /// The clamped door count.
    pub fn doors(&self) -> u32 {
        self.doors.max(2)
    }

    /// The correct door at `level` — a pure integer function of the maze
    /// seed (splitmix64 of `maze_seed ⊕ level`), so the maze is fixed across
    /// campaign seeds and re-derivable anywhere (guest and host agree by
    /// construction).
    pub fn correct_door(&self, level: u32) -> u32 {
        (splitmix64(self.maze_seed ^ u64::from(level)) % u64::from(self.doors())) as u32
    }
}

/// The walker's full state — the maze's whole observable world. `x` and
/// `level` are the bounded integers the workload reports as its X/Y state
/// registers; the two absorbing conditions are encoded in the observable
/// coordinates (see [`MazeState::x_register`]).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct MazeState {
    /// Corridor position, `0 ..= width - 1` (0 at the goal tile).
    pub x: u32,
    /// Corridor level, `0 ..= levels - 1`; at the goal, `levels`.
    pub level: u32,
    /// Reached the goal (the last level was completed) — the one absorbing
    /// state.
    pub goal: bool,
}

impl MazeState {
    /// The walker's start state.
    pub fn start() -> Self {
        MazeState::default()
    }

    /// The X state-register value: the corridor position (`0` at the goal
    /// tile).
    pub fn x_register(&self) -> u64 {
        u64::from(self.x)
    }

    /// The Y state-register value: the level, or `levels` at the goal tile.
    pub fn y_register(&self) -> u64 {
        u64::from(self.level)
    }

    /// Whether the walk is absorbed (the goal) — no further byte changes it.
    pub fn absorbed(&self) -> bool {
        self.goal
    }
}

/// One walk step: consume one entropy byte and return the successor state. A
/// pure function — same `(spec, state, byte)` ⇒ same successor; total on all
/// 256 byte values and on any (clamped) spec.
///
/// - The **goal** (absorbed) returns itself.
/// - **Corridor** (`x < width - 1`): `byte % 8` drives a rightward drift —
///   `0..=4` step right, `5..=6` stay, `7` step left (floored at 0).
/// - **Junction** (`x = width - 1`): `byte % doors` picks a door. The
///   level's correct door advances to `(0, level + 1)` — or the goal after
///   the last level; a wrong door **resets the walk to the maze start**
///   `(0, 0)`.
pub fn step(spec: &MazeSpec, state: MazeState, byte: u8) -> MazeState {
    if state.absorbed() {
        return state;
    }
    let width = spec.width();
    // Defensive clamp: a state from outside the reachable set (hand-built)
    // is treated as its in-corridor clamp rather than panicking.
    let x = state.x.min(width - 1);
    let level = state.level.min(spec.levels() - 1);
    if x < width - 1 {
        let x = match byte % 8 {
            0..=4 => x + 1,
            5 | 6 => x,
            _ => x.saturating_sub(1),
        };
        return MazeState {
            x,
            level,
            goal: false,
        };
    }
    // The junction.
    let door = u32::from(byte) % spec.doors();
    if door == spec.correct_door(level) {
        if level + 1 == spec.levels() {
            MazeState {
                x: 0,
                level: spec.levels(),
                goal: true,
            }
        } else {
            MazeState {
                x: 0,
                level: level + 1,
                goal: false,
            }
        }
    } else {
        // The junction reset: back to the maze start, keep walking.
        MazeState::start()
    }
}

/// Walk `bytes` from `state`, returning every successor in order (one per
/// byte, absorbing states included — the emission stream mirrors the walk
/// one-to-one).
pub fn walk(spec: &MazeSpec, mut state: MazeState, bytes: &[u8]) -> Vec<MazeState> {
    let mut out = Vec::with_capacity(bytes.len());
    for &b in bytes {
        state = step(spec, state, b);
        out.push(state);
    }
    out
}

/// The exact count of distinct observable `(x_register, y_register)` tiles
/// reachable from the start — the gate report's documented frontier, against
/// which the non-vacuity claim (“the baseline plateaus **below** the
/// reachable frontier”) is checked rather than asserted in prose.
///
/// `width` corridor tiles per level, plus the goal tile.
pub fn reachable_cells(spec: &MazeSpec) -> u64 {
    u64::from(spec.levels()) * u64::from(spec.width()) + 1
}

/// A byte stream that walks the maze straight to the goal: drift right to
/// each junction, then take the level's correct door. The test oracle for
/// “the goal is reachable”, and the depth-`d` witness prefix for any `d`.
pub fn oracle_bytes(spec: &MazeSpec) -> Vec<u8> {
    let mut out = Vec::new();
    for level in 0..spec.levels() {
        // `byte % 8 == 0` steps right; width-1 steps reach the junction tile
        // (`x = width - 1`) from the level's start (`x = 0`).
        out.extend(std::iter::repeat_n(0u8, (spec.width() - 1) as usize));
        // A junction byte drawing exactly the correct door (doors ≤ 256 - 8:
        // pick the smallest byte ≡ door (mod doors); any representative works).
        out.push(spec.correct_door(level) as u8);
    }
    out
}

/// SplitMix64 — the crate's one integer mixing primitive (used only to derive
/// the per-level correct door from the manifest's `maze_seed`; the walk draws
/// no entropy of its own).
fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeSet;

    fn spec() -> MazeSpec {
        MazeSpec::small()
    }

    /// A deterministic pseudo-entropy stream for the plateau measurements —
    /// seeded, integer-only (conventions rule 4).
    fn stream(seed: u64, len: usize) -> Vec<u8> {
        let mut z = seed;
        (0..len)
            .map(|_| {
                z = splitmix64(z);
                (z & 0xff) as u8
            })
            .collect()
    }

    /// The oracle stream reaches the goal — the maze is solvable, and its
    /// depth witness is exactly `levels`.
    #[test]
    fn oracle_walk_reaches_the_goal() {
        let s = spec();
        let states = walk(&s, MazeState::start(), &oracle_bytes(&s));
        let last = states.last().copied().unwrap();
        assert!(last.goal);
        assert_eq!(last.y_register(), u64::from(s.levels()));
    }

    /// A fixed stream drives a fixed path (the determinism pin), and a wrong
    /// door resets the walk to the maze start.
    #[test]
    fn fixed_stream_fixed_path_and_wrong_doors_reset() {
        let s = spec();
        let bytes = stream(7, 64);
        let a = walk(&s, MazeState::start(), &bytes);
        let b = walk(&s, MazeState::start(), &bytes);
        assert_eq!(a, b);

        // Take a wrong door explicitly: right to the junction, then a wrong door.
        let wrong = (s.correct_door(0) + 1) % s.doors();
        let mut bytes: Vec<u8> = std::iter::repeat_n(0u8, (s.width() - 1) as usize).collect();
        bytes.push(wrong as u8);
        let states = walk(&s, MazeState::start(), &bytes);
        assert_eq!(
            *states.last().unwrap(),
            MazeState::start(),
            "a wrong door resets to (0, 0)"
        );
        // The goal, once reached, absorbs.
        let mut bytes = oracle_bytes(&s);
        bytes.extend([0u8, 3, 250]);
        let states = walk(&s, MazeState::start(), &bytes);
        let at_goal = *states.last().unwrap();
        assert!(at_goal.goal);
        assert!(
            states[oracle_bytes(&s).len() - 1..]
                .iter()
                .all(|st| *st == at_goal)
        );
    }

    /// `reachable_cells` is exact: exhaustive forward closure over the real
    /// transition function (all 256 bytes at every frontier state) reaches
    /// exactly the formula's count of distinct observable tiles.
    #[test]
    fn reachable_cells_matches_exhaustive_closure() {
        let s = spec();
        let mut seen: BTreeSet<MazeState> = BTreeSet::new();
        let mut frontier = vec![MazeState::start()];
        seen.insert(MazeState::start());
        while let Some(st) = frontier.pop() {
            for byte in 0..=255u8 {
                let nxt = step(&s, st, byte);
                if seen.insert(nxt) {
                    frontier.push(nxt);
                }
            }
        }
        let tiles: BTreeSet<(u64, u64)> = seen
            .iter()
            .map(|st| (st.x_register(), st.y_register()))
            .collect();
        assert_eq!(tiles.len() as u64, reachable_cells(&s));
    }

    /// The non-vacuity property, measured at the campaign's per-rollout
    /// budget: independent seeded random walks plateau well below the goal
    /// (every wrong door re-climbs the whole gauntlet, so depth decays
    /// geometrically), while the oracle depth witness is the full `levels`.
    /// Deterministic measurement over 64 fixed seeds × one 48-step rollout
    /// (the smoke config's `steps_per_rollout`): no walk reaches the goal,
    /// none gets within one level of it, and the median stays shallow.
    #[test]
    fn random_walks_plateau_well_short_of_the_goal() {
        let s = spec();
        let budget = 48;
        let mut depths: Vec<u64> = (0..64u64)
            .map(|seed| {
                walk(&s, MazeState::start(), &stream(seed, budget))
                    .iter()
                    .map(|st| st.y_register())
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        depths.sort_unstable();
        let max_depth = depths[63];
        let median = depths[32];
        assert!(
            max_depth + 1 < u64::from(s.levels()),
            "random plateau reached depth {max_depth} of {} — the maze is too easy \
             (the non-vacuity property fails)",
            s.levels()
        );
        assert!(
            median <= 2,
            "random median depth {median} — the maze is too easy"
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// The transition function is total and closed: from any reachable
        /// state, any byte yields a state whose registers stay inside the
        /// documented observable bounds.
        #[test]
        fn step_is_total_and_bounded(byte: u8, walk_len in 0usize..128, seed: u64) {
            let s = spec();
            let bytes = stream(seed, walk_len);
            let state = walk(&s, MazeState::start(), &bytes).last().copied()
                .unwrap_or(MazeState::start());
            let nxt = step(&s, state, byte);
            prop_assert!(nxt.x_register() < u64::from(s.width() + s.doors()));
            prop_assert!(nxt.y_register() <= u64::from(s.levels()));
            // Absorbing states are fixed points.
            if state.absorbed() {
                prop_assert_eq!(nxt, state);
            }
        }

        /// Same prefix ⇒ same walk (purity), and a walk's depth is monotone
        /// in its prefix length.
        #[test]
        fn walks_are_pure_and_depth_monotone(seed: u64, len in 1usize..96) {
            let s = spec();
            let bytes = stream(seed, len);
            let full = walk(&s, MazeState::start(), &bytes);
            let half = walk(&s, MazeState::start(), &bytes[..len / 2]);
            prop_assert_eq!(&full[..len / 2], &half[..]);
            let depth = |states: &[MazeState]| {
                states.iter().map(|st| st.y_register()).max().unwrap_or(0)
            };
            prop_assert!(depth(&full) >= depth(&half));
        }

        /// The per-level correct door is stable and in range for any spec.
        #[test]
        fn correct_door_is_stable_and_in_range(maze_seed: u64, level in 0u32..64, doors in 2u32..12) {
            let s = MazeSpec { width: 4, levels: 64, doors, maze_seed };
            let d = s.correct_door(level);
            prop_assert!(d < doors);
            prop_assert_eq!(d, s.correct_door(level));
        }
    }
}
