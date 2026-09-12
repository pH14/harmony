// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fmt;

use crate::billboard::{BillboardError, BillboardLayout};
use crate::chord::ChordAlphabet;
use crate::core_seam::Core;
use crate::ram::{self, RamError, SmbState};
use crate::regs;

pub trait Harness {
    type Error: fmt::Debug;

    fn entropy_byte(&mut self) -> Result<u8, Self::Error>;
    fn state_set(&mut self, reg: u32, value: u64) -> Result<(), Self::Error>;
    fn state_max(&mut self, reg: u32, value: u64) -> Result<(), Self::Error>;
    fn reachable(&mut self, point: u32) -> Result<(), Self::Error>;
}

#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub window: u32,
    pub x_bucket_px: u32,
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

#[derive(Debug)]
pub enum AgentError<E> {
    Harness(E),
    Ram(RamError),
    Billboard(BillboardError),
    SerializeFailed,
    WorkRamFailed,
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StepReport {
    pub frame: u32,
    pub joypad: u8,
    pub state: Option<SmbState>,
}

pub struct Agent<C: Core> {
    core: C,
    cfg: AgentConfig,
    layout: BillboardLayout,
    frame: u32,
    chord: u8,
    start_ordinal: Option<u64>,
    level_cleared_fired: bool,
    world_two_fired: bool,
}

impl<C: Core> Agent<C> {
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

    pub fn layout(&self) -> BillboardLayout {
        self.layout
    }

    pub fn frame(&self) -> u32 {
        self.frame
    }

    pub fn core_mut(&mut self) -> &mut C {
        &mut self.core
    }

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

    pub fn step<H: Harness>(
        &mut self,
        harness: &mut H,
        billboard: &mut [u8],
    ) -> Result<StepReport, AgentError<H::Error>> {
        let window_boundary = self.frame.is_multiple_of(self.cfg.window);

        if window_boundary {
            let byte = harness.entropy_byte().map_err(AgentError::Harness)?;
            self.chord = self.cfg.alphabet.decode(byte);
        }

        self.layout
            .write_header(billboard, self.frame, self.chord)
            .map_err(AgentError::Billboard)?;
        if !self.core.serialize(self.layout.savestate_mut(billboard)) {
            return Err(AgentError::SerializeFailed);
        }
        if !self.core.read_work_ram(self.layout.work_ram_mut(billboard)) {
            return Err(AgentError::WorkRamFailed);
        }

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

        harness
            .state_set(regs::REG_FRAME, u64::from(self.frame))
            .map_err(AgentError::Harness)?;

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
        agent.step(&mut h, &mut buf).unwrap();
        assert!(h.reachables.is_empty());

        agent.core_mut().ram_mut()[addr::LEVEL_NUMBER] = 1;
        agent.step(&mut h, &mut buf).unwrap();
        assert_eq!(h.reachables, vec![regs::POINT_LEVEL_CLEARED]);
        agent.step(&mut h, &mut buf).unwrap();
        assert_eq!(h.reachables.len(), 1, "fires once");

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
