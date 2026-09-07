// SPDX-License-Identifier: AGPL-3.0-or-later
//! The per-frame agent loop: the `retro_run` counter is the frame clock, and
//! each vblank the agent (1) draws chord inputs (one entropy byte per input
//! window), (2) publishes the billboard *before* the frame's `retro_run`,
//! (3) emits state registers once per window and the frame clock every vblank,
//! and (4) marks legibility events — task 86 §play-agent, in that order.
//!
//! The loop is generic over the [`Core`] seam (mock in tests, libretro in the
//! guest) and the [`Harness`] seam (the SDK in the guest, a recording fake in
//! tests), so every decision here is portable, deterministic, and a pure
//! function of the entropy bytes the harness supplies.

use std::fmt;

use crate::billboard::{BillboardError, BillboardLayout};
use crate::chord::ChordAlphabet;
use crate::core_seam::Core;
use crate::ram::{self, RamError, SmbState};
use crate::regs;

/// The SDK-facing seam: exactly the verbs task 86 permits (`state_set`/
/// `state_max`/`assert_reachable`/`entropy_fill` — nothing else, R-L2). The
/// binary implements it over `harmony_sdk::Sdk`; tests implement it over a
/// recording fake with a scripted entropy stream.
pub trait Harness {
    /// The transport-level error the guest surfaces loudly (never swallowed —
    /// a swallowed emission reads as "never happened" and makes gates pass
    /// vacuously; the `sdk-demo` discipline).
    type Error: fmt::Debug;

    /// Draw one byte of decision entropy from the seeded stream.
    fn entropy_byte(&mut self) -> Result<u8, Self::Error>;
    /// Report a state register (IJON assign).
    fn state_set(&mut self, reg: u32, value: u64) -> Result<(), Self::Error>;
    /// Report a keep-max state register (the host mints novelty on increase).
    fn state_max(&mut self, reg: u32, value: u64) -> Result<(), Self::Error>;
    /// Fire a reachability legibility marker.
    fn reachable(&mut self, point: u32) -> Result<(), Self::Error>;
}

/// The agent's manifest parameters (task 86: alphabet, weights, and `W` are
/// manifest parameters — tuning *them* is legitimate input shaping; tuning the
/// game is impossible).
#[derive(Clone, Debug)]
pub struct AgentConfig {
    /// The input window `W` in frames: one entropy byte (one chord) per `W`
    /// frames. Suggested 8–24.
    pub window: u32,
    /// The x-bucket width in pixels (~128–256): `REG_X_BUCKET = x_abs / bucket`.
    pub x_bucket_px: u32,
    /// The weighted chord alphabet.
    pub alphabet: ChordAlphabet,
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            window: 12,
            x_bucket_px: 128,
            alphabet: ChordAlphabet::smb_default(),
        }
    }
}

/// Why a frame step failed. Every variant is fatal to the run: a torn
/// billboard or a swallowed emission would corrupt the record silently, so the
/// agent stops loudly instead (the guest init maps that to a crash terminal).
#[derive(Debug)]
pub enum AgentError<E> {
    /// A harness (SDK/transport) verb failed.
    Harness(E),
    /// The core's work RAM did not decode.
    Ram(RamError),
    /// The billboard buffer did not fit the layout.
    Billboard(BillboardError),
    /// The core failed to serialize its savestate.
    SerializeFailed,
    /// The core could not expose its work RAM.
    WorkRamFailed,
    /// The configured window is zero (a config error caught at construction,
    /// kept as an error rather than a panic per rule 4).
    ZeroWindow,
}

impl<E: fmt::Debug> fmt::Display for AgentError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AgentError::Harness(e) => write!(f, "harness verb failed: {e:?}"),
            AgentError::Ram(e) => write!(f, "work RAM decode failed: {e}"),
            AgentError::Billboard(e) => write!(f, "billboard write failed: {e}"),
            AgentError::SerializeFailed => write!(f, "core serialize failed"),
            AgentError::WorkRamFailed => write!(f, "core work RAM unavailable"),
            AgentError::ZeroWindow => write!(f, "input window must be at least 1 frame"),
        }
    }
}

impl<E: fmt::Debug> std::error::Error for AgentError<E> {}

/// What one frame step did — the smoke mode prints these, and the portable
/// tests assert the input tape and register emissions against them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StepReport {
    /// The frame this step published and ran (the billboard's stamped frame).
    pub frame: u32,
    /// The joypad byte held for this frame.
    pub joypad: u8,
    /// The decoded state, present on window-boundary frames (when registers
    /// were emitted).
    pub state: Option<SmbState>,
}

/// The play-agent's frame-stepped brain over a [`Core`].
pub struct Agent<C: Core> {
    core: C,
    cfg: AgentConfig,
    layout: BillboardLayout,
    frame: u32,
    chord: u8,
    /// The `(world, level)` ordinal at the first gameplay observation — the
    /// baseline the level-cleared marker compares against.
    start_ordinal: Option<u64>,
    level_cleared_fired: bool,
    world_two_fired: bool,
}

impl<C: Core> Agent<C> {
    /// Freeze the billboard layout from the core's serialize size and build
    /// the agent. The first window's chord is drawn on the first step.
    pub fn new(
        mut core: C,
        cfg: AgentConfig,
    ) -> Result<Self, AgentError<std::convert::Infallible>> {
        if cfg.window == 0 {
            return Err(AgentError::ZeroWindow);
        }
        let layout = BillboardLayout::new(core.serialize_size()).map_err(AgentError::Billboard)?;
        Ok(Agent {
            core,
            cfg,
            layout,
            frame: 0,
            chord: 0,
            start_ordinal: None,
            level_cleared_fired: false,
            world_two_fired: false,
        })
    }

    /// The frozen billboard layout (the binary sizes and publishes the pinned
    /// buffer from this).
    pub fn layout(&self) -> BillboardLayout {
        self.layout
    }

    /// The frames stepped so far (the next frame to publish).
    pub fn frame(&self) -> u32 {
        self.frame
    }

    /// Immutable access to the core (tests inspect the mock's RAM).
    pub fn core_mut(&mut self) -> &mut C {
        &mut self.core
    }

    /// Fill the billboard **without stepping** (round-8 P1: the seal-point
    /// billboard must never be zeros): header for the current frame with a
    /// neutral joypad, the core's real savestate, the real work RAM — proving
    /// `retro_serialize` and RAM access work *before* `setup_complete` seals
    /// the base, so every branch inherits a valid, decodable billboard.
    /// Returns the decoded state so the caller can also verify the seal point
    /// is in-gameplay (the vacuity check, in-guest). Draws no entropy and
    /// emits nothing; the first `step` deterministically rewrites the same
    /// frame with its real chord.
    pub fn prime_billboard(
        &mut self,
        billboard: &mut [u8],
    ) -> Result<SmbState, AgentError<std::convert::Infallible>> {
        self.layout
            .write_header(billboard, self.frame, 0)
            .map_err(AgentError::Billboard)?;
        if !self.core.serialize(self.layout.savestate_mut(billboard)) {
            return Err(AgentError::SerializeFailed);
        }
        if !self.core.read_work_ram(self.layout.work_ram_mut(billboard)) {
            return Err(AgentError::WorkRamFailed);
        }
        ram::decode(self.layout.work_ram_mut(billboard)).map_err(AgentError::Ram)
    }

    /// Run one frame: draw (on window boundaries), publish the billboard,
    /// emit registers + the frame clock, mark legibility events, then
    /// `retro_run`. `billboard` is the pinned buffer (at least
    /// [`BillboardLayout::total_len`] bytes).
    pub fn step<H: Harness>(
        &mut self,
        harness: &mut H,
        billboard: &mut [u8],
    ) -> Result<StepReport, AgentError<H::Error>> {
        let window_boundary = self.frame.is_multiple_of(self.cfg.window);

        // (1) Draw this window's chord — one entropy byte per input window,
        // decoded against the weighted alphabet, held for the whole window.
        if window_boundary {
            let byte = harness.entropy_byte().map_err(AgentError::Harness)?;
            self.chord = self.cfg.alphabet.decode(byte);
        }

        // (3 in spec order, done first here so the header can never disagree
        // with what retro_run will see) Publish the billboard *before* the
        // frame's retro_run: header (frame + this frame's joypad byte), the
        // core's full savestate, the 2 KiB console work RAM.
        self.layout
            .write_header(billboard, self.frame, self.chord)
            .map_err(AgentError::Billboard)?;
        if !self.core.serialize(self.layout.savestate_mut(billboard)) {
            return Err(AgentError::SerializeFailed);
        }
        if !self.core.read_work_ram(self.layout.work_ram_mut(billboard)) {
            return Err(AgentError::WorkRamFailed);
        }

        // (2) Emit state registers once per window, decoded from the work RAM
        // just published (the state as of the previous frame's end).
        let mut state = None;
        if window_boundary {
            let s = ram::decode(self.layout.work_ram_mut(billboard)).map_err(AgentError::Ram)?;
            harness
                .state_set(regs::REG_GAME_MODE, u64::from(s.game_mode))
                .map_err(AgentError::Harness)?;
            harness
                .state_set(regs::REG_WORLD, u64::from(s.world))
                .map_err(AgentError::Harness)?;
            harness
                .state_set(regs::REG_LEVEL, u64::from(s.level))
                .map_err(AgentError::Harness)?;
            harness
                .state_set(
                    regs::REG_X_BUCKET,
                    u64::from(s.x_abs / self.cfg.x_bucket_px.max(1)),
                )
                .map_err(AgentError::Harness)?;
            harness
                .state_set(regs::REG_POWERUP, u64::from(s.powerup))
                .map_err(AgentError::Harness)?;

            // (4) Depth + legibility markers, gameplay only (title-screen
            // bytes are menu state, not progress).
            if s.in_gameplay() {
                let ordinal = s.depth_ordinal();
                harness
                    .state_max(regs::REG_DEPTH, ordinal)
                    .map_err(AgentError::Harness)?;
                let start = *self.start_ordinal.get_or_insert(ordinal);
                if ordinal > start && !self.level_cleared_fired {
                    harness
                        .reachable(regs::POINT_LEVEL_CLEARED)
                        .map_err(AgentError::Harness)?;
                    self.level_cleared_fired = true;
                }
                if s.world >= 1 && !self.world_two_fired {
                    harness
                        .reachable(regs::POINT_WORLD_TWO)
                        .map_err(AgentError::Harness)?;
                    self.world_two_fired = true;
                }
            }
            state = Some(s);
        }

        // The frame clock, every vblank — the Moment task 87 addresses this
        // frame's billboard by, emitted after the billboard bytes are in
        // place so the read at that Moment sees this frame.
        harness
            .state_set(regs::REG_FRAME, u64::from(self.frame))
            .map_err(AgentError::Harness)?;

        // Run the frame under the held chord.
        let report = StepReport {
            frame: self.frame,
            joypad: self.chord,
            state,
        };
        self.core.run_frame(self.chord);
        self.frame = self.frame.wrapping_add(1);
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_seam::MockCore;
    use crate::ram::addr;

    /// A recording harness with a scripted entropy stream.
    #[derive(Default)]
    pub struct FakeHarness {
        pub entropy: Vec<u8>,
        cursor: usize,
        pub sets: Vec<(u32, u64)>,
        pub maxes: Vec<(u32, u64)>,
        pub reachables: Vec<u32>,
    }

    impl FakeHarness {
        pub fn scripted(entropy: Vec<u8>) -> Self {
            FakeHarness {
                entropy,
                ..FakeHarness::default()
            }
        }
    }

    impl Harness for FakeHarness {
        type Error = &'static str;
        fn entropy_byte(&mut self) -> Result<u8, Self::Error> {
            let b = *self.entropy.get(self.cursor).ok_or("entropy exhausted")?;
            self.cursor += 1;
            Ok(b)
        }
        fn state_set(&mut self, reg: u32, value: u64) -> Result<(), Self::Error> {
            self.sets.push((reg, value));
            Ok(())
        }
        fn state_max(&mut self, reg: u32, value: u64) -> Result<(), Self::Error> {
            self.maxes.push((reg, value));
            Ok(())
        }
        fn reachable(&mut self, point: u32) -> Result<(), Self::Error> {
            self.reachables.push(point);
            Ok(())
        }
    }

    fn small_cfg(window: u32) -> AgentConfig {
        AgentConfig {
            window,
            ..AgentConfig::default()
        }
    }

    #[test]
    fn draws_one_byte_per_window_and_holds_the_chord() {
        let cfg = small_cfg(4);
        let alphabet = cfg.alphabet.clone();
        let mut agent = Agent::new(MockCore::in_gameplay(), cfg).unwrap();
        let mut h = FakeHarness::scripted(vec![0, 56, 200]);
        let mut buf = vec![0u8; agent.layout().total_len()];
        let mut tape = Vec::new();
        for _ in 0..12 {
            tape.push(agent.step(&mut h, &mut buf).unwrap().joypad);
        }
        // Three windows of four frames, each holding its decoded chord.
        let expected: Vec<u8> = [0u8, 56, 200]
            .into_iter()
            .flat_map(|b| std::iter::repeat_n(alphabet.decode(b), 4))
            .collect();
        assert_eq!(tape, expected);
    }

    #[test]
    fn emits_registers_once_per_window_and_frame_every_vblank() {
        let mut agent = Agent::new(MockCore::in_gameplay(), small_cfg(3)).unwrap();
        let mut h = FakeHarness::scripted(vec![0; 8]);
        let mut buf = vec![0u8; agent.layout().total_len()];
        for _ in 0..6 {
            agent.step(&mut h, &mut buf).unwrap();
        }
        let frames: Vec<u64> = h
            .sets
            .iter()
            .filter(|(r, _)| *r == regs::REG_FRAME)
            .map(|(_, v)| *v)
            .collect();
        assert_eq!(frames, vec![0, 1, 2, 3, 4, 5]);
        let modes: Vec<u64> = h
            .sets
            .iter()
            .filter(|(r, _)| *r == regs::REG_GAME_MODE)
            .map(|(_, v)| *v)
            .collect();
        assert_eq!(modes.len(), 2, "two window boundaries in six frames");
        // Depth is emitted via state_max on each boundary during gameplay.
        assert_eq!(h.maxes.len(), 2);
        assert!(h.maxes.iter().all(|(r, _)| *r == regs::REG_DEPTH));
    }

    #[test]
    fn billboard_carries_the_frame_and_joypad_it_will_run() {
        let mut agent = Agent::new(MockCore::in_gameplay(), small_cfg(2)).unwrap();
        let mut h = FakeHarness::scripted(vec![0; 8]);
        let mut buf = vec![0u8; agent.layout().total_len()];
        for expected_frame in 0..4u32 {
            let report = agent.step(&mut h, &mut buf).unwrap();
            let frame = u32::from_le_bytes(buf[8..12].try_into().unwrap());
            assert_eq!(frame, expected_frame);
            assert_eq!(buf[12], report.joypad);
            assert_eq!(&buf[0..4], b"HBBD");
        }
    }

    #[test]
    fn level_cleared_and_world_two_fire_once() {
        let mut agent = Agent::new(MockCore::in_gameplay(), small_cfg(1)).unwrap();
        let mut h = FakeHarness::scripted(vec![0; 32]);
        let mut buf = vec![0u8; agent.layout().total_len()];
        agent.step(&mut h, &mut buf).unwrap(); // baseline: 1-1 observed
        assert!(h.reachables.is_empty());

        // Clear a level: 1-1 -> 1-2.
        agent.core_mut().ram_mut()[addr::LEVEL_NUMBER] = 1;
        agent.step(&mut h, &mut buf).unwrap();
        assert_eq!(h.reachables, vec![regs::POINT_LEVEL_CLEARED]);
        agent.step(&mut h, &mut buf).unwrap();
        assert_eq!(h.reachables.len(), 1, "fires once");

        // Warp to world 5 (index 4): world-two marker fires (warp zones are
        // real progress).
        agent.core_mut().ram_mut()[addr::WORLD_NUMBER] = 4;
        agent.step(&mut h, &mut buf).unwrap();
        assert_eq!(
            h.reachables,
            vec![regs::POINT_LEVEL_CLEARED, regs::POINT_WORLD_TWO]
        );
        agent.step(&mut h, &mut buf).unwrap();
        assert_eq!(h.reachables.len(), 2, "both fire once");
    }

    #[test]
    fn title_screen_emits_registers_but_no_depth_or_markers() {
        let mut agent = Agent::new(MockCore::new(), small_cfg(1)).unwrap();
        let mut h = FakeHarness::scripted(vec![0; 4]);
        let mut buf = vec![0u8; agent.layout().total_len()];
        for _ in 0..3 {
            agent.step(&mut h, &mut buf).unwrap();
        }
        assert!(h.maxes.is_empty(), "no depth outside gameplay");
        assert!(h.reachables.is_empty());
        assert!(h.sets.iter().any(|(r, _)| *r == regs::REG_GAME_MODE));
    }

    #[test]
    fn a_failed_entropy_draw_is_fatal_and_loud() {
        let mut agent = Agent::new(MockCore::in_gameplay(), small_cfg(1)).unwrap();
        let mut h = FakeHarness::scripted(vec![]);
        let mut buf = vec![0u8; agent.layout().total_len()];
        assert!(matches!(
            agent.step(&mut h, &mut buf),
            Err(AgentError::Harness("entropy exhausted"))
        ));
    }

    #[test]
    fn a_short_billboard_buffer_is_fatal() {
        let mut agent = Agent::new(MockCore::in_gameplay(), small_cfg(1)).unwrap();
        let mut h = FakeHarness::scripted(vec![0; 4]);
        let mut buf = vec![0u8; agent.layout().total_len() - 1];
        assert!(matches!(
            agent.step(&mut h, &mut buf),
            Err(AgentError::Billboard(BillboardError::BufferTooSmall { .. }))
        ));
    }

    /// Round-8 P1: the seal-point billboard is primed with a real frame
    /// (header + savestate + work RAM) without stepping — never zeros — and
    /// the first step still stamps frame 0.
    #[test]
    fn prime_billboard_fills_a_valid_frame_without_stepping() {
        let mut agent = Agent::new(MockCore::in_gameplay(), small_cfg(4)).unwrap();
        let mut buf = vec![0u8; agent.layout().total_len()];
        let state = agent.prime_billboard(&mut buf).unwrap();
        assert!(state.in_gameplay(), "the vacuity check input");
        assert_eq!(&buf[0..4], b"HBBD");
        assert_eq!(agent.frame(), 0, "prime must not step");
        let ss_start = crate::billboard::HEADER_LEN;
        assert!(
            buf[ss_start..ss_start + 8].iter().any(|&b| b != 0),
            "the savestate region carries the core's real serialize output"
        );
        let mut h = FakeHarness::scripted(vec![0]);
        assert_eq!(agent.step(&mut h, &mut buf).unwrap().frame, 0);
    }

    #[test]
    fn zero_window_is_rejected_at_construction() {
        assert!(matches!(
            Agent::new(MockCore::new(), small_cfg(0)),
            Err(AgentError::ZeroWindow)
        ));
    }
}
