// SPDX-License-Identifier: AGPL-3.0-or-later
//! Nova the Squirrel payload mode for the Consonance guest play-agent.
//!
//! The guest asks the SDK for opaque two-byte controller chords, advances the
//! libretro core, publishes raw frame memory, and yields at each completed
//! chord. Snapshotting and observation decoding remain entirely host owned:
//! `frame_complete` is the clean Consonance lifecycle boundary.

use std::fmt;

use harmony_sdk::Point;

use crate::{
    billboard::{BillboardError, NOVA_SAVE_RAM_LEN, NovaBillboardLayout},
    core_seam::Core,
};

/// Nova's 2 KiB NES work-RAM window.
pub const WORK_RAM_LEN: usize = 2 * 1024;
/// Nova's cartridge-backed save-RAM window under the pinned QuickNES core.
pub const SAVE_RAM_LEN: usize = 8 * 1024;
/// Largest accepted controller hold, identical to `machine::nes`.
pub const MAX_HOLD_FRAMES: u8 = 120;

const PLAYER_X_LOW: usize = 0x25;
const PLAYER_X_HIGH: usize = 0x26;
const PLAYER_Y_HIGH: usize = 0x27;
const PLAYER_Y_LOW: usize = 0x28;
const PLAYER_HEALTH: usize = 0x4b;
const LEVEL_NUMBER: usize = 0xa7;
const STARTED_LEVEL_NUMBER: usize = 0xa8;
const CHIP_COUNT: usize = 0x508;
const CHIPS_NEEDED: usize = 0x509;
const SAVE_RAM_BASE: usize = 0x6000;
const PLAYER_ABILITY: usize = 0x7200 - SAVE_RAM_BASE;
const LEVEL_CLEARED: usize = 0x7f1f - SAVE_RAM_BASE;
const LEVEL_AVAILABLE: usize = 0x7f27 - SAVE_RAM_BASE;
const COLLECTIBLE_BITS: usize = 0x7f2f - SAVE_RAM_BASE;
const BITMAP_LEN: usize = 8;

const A: u8 = 1 << 0;
const START: u8 = 1 << 3;
const UP: u8 = 1 << 4;

const BOOT_ACTIONS: &[(u8, u8)] = &[
    (0, 60),
    (START, 6),
    (0, 114),
    (START, 6),
    (0, 54),
    (A, 6),
    (0, 54),
    (UP, 6),
    (0, 6),
    (UP, 6),
    (0, 6),
    (UP, 6),
    (0, 54),
    (A, 6),
    (0, 60),
];

/// Nova-specific SDK register and marker catalog.
pub mod regs {
    use super::Point;

    /// Selected campaign level, zero based.
    pub const REG_STARTED_LEVEL: u32 = 1;
    /// Current internal map number.
    pub const REG_LEVEL: u32 = 2;
    /// Player X in 32-pixel buckets.
    pub const REG_X_BUCKET: u32 = 3;
    /// Player Y in 32-pixel buckets.
    pub const REG_Y_BUCKET: u32 = 4;
    /// Current health in half-hearts.
    pub const REG_HEALTH: u32 = 5;
    /// Current copied ability.
    pub const REG_ABILITY: u32 = 6;
    /// Durable cleared-level count.
    pub const REG_CLEARED: u32 = 7;
    /// Unlocked-level count.
    pub const REG_AVAILABLE: u32 = 8;
    /// Durable collectible count.
    pub const REG_COLLECTIBLES: u32 = 9;
    /// Cumulative emulated frames.
    pub const REG_FRAME: u32 = 10;
    /// Guest-physical address of the QuickNES billboard.
    pub const REG_BILLBOARD_GPA: u32 = 11;
    /// Byte length of the QuickNES billboard.
    pub const REG_BILLBOARD_LEN: u32 = 12;
    /// Exact player X coordinate in whole pixels.
    pub const REG_X: u32 = 13;
    /// Exact player Y coordinate in whole pixels.
    pub const REG_Y: u32 = 14;
    /// Puzzle chips currently carried.
    pub const REG_CHIPS: u32 = 15;
    /// Puzzle chips required by the current map.
    pub const REG_CHIPS_NEEDED: u32 = 16;
    /// Whether the game requested an internal map reload.
    pub const REG_LEVEL_RELOAD: u32 = 17;

    /// First durable level clear beyond guest genesis.
    pub const POINT_LEVEL_CLEARED: u32 = 1;
    /// First durable collectible beyond guest genesis.
    pub const POINT_COLLECTIBLE: u32 = 2;
    /// First copied ability beyond guest genesis.
    pub const POINT_ABILITY: u32 = 3;

    /// Catalog declared once when Nova payload mode initializes the SDK.
    pub const CATALOG: &[Point] = &[
        Point::state(REG_STARTED_LEVEL, "nova_started_level"),
        Point::state(REG_LEVEL, "nova_level"),
        Point::state(REG_X_BUCKET, "nova_x_bucket"),
        Point::state(REG_Y_BUCKET, "nova_y_bucket"),
        Point::state(REG_HEALTH, "nova_health"),
        Point::state(REG_ABILITY, "nova_ability"),
        Point::state(REG_CLEARED, "nova_cleared"),
        Point::state(REG_AVAILABLE, "nova_available"),
        Point::state(REG_COLLECTIBLES, "nova_collectibles"),
        Point::state(REG_FRAME, "nova_frame"),
        Point::state(REG_BILLBOARD_GPA, "nova_billboard_gpa"),
        Point::state(REG_BILLBOARD_LEN, "nova_billboard_len"),
        Point::state(REG_X, "nova_x"),
        Point::state(REG_Y, "nova_y"),
        Point::state(REG_CHIPS, "nova_chips"),
        Point::state(REG_CHIPS_NEEDED, "nova_chips_needed"),
        Point::state(REG_LEVEL_RELOAD, "nova_level_reload"),
        Point::reachable(POINT_LEVEL_CLEARED, "nova_level_cleared"),
        Point::reachable(POINT_COLLECTIBLE, "nova_collectible"),
        Point::reachable(POINT_ABILITY, "nova_ability_acquired"),
    ];
}

/// Source-derived Nova mechanical state exposed to the host as SDK markers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NovaState {
    /// Current internal map number.
    pub level: u8,
    /// Player-selected campaign level number.
    pub started_level: u8,
    /// Player X in whole pixels.
    pub x: u16,
    /// Player Y in whole pixels.
    pub y: u16,
    /// Current health in half-hearts.
    pub health: u8,
    /// Puzzle chips currently carried.
    pub chips: u8,
    /// Puzzle chips required by the current map.
    pub chips_needed: u8,
    /// Current copied ability.
    pub ability: u8,
    /// Durable cleared-level count.
    pub cleared: u8,
    /// Unlocked-level count.
    pub available: u8,
    /// Durable collectible count.
    pub collectibles: u8,
}

impl NovaState {
    fn dead(self) -> bool {
        self.health == 0
    }
}

/// SDK operations needed by Nova payload mode.
pub trait NovaChannel {
    /// Channel-specific failure.
    type Error;

    /// Consume one exact two-byte `[buttons, hold_frames]` payload.
    fn payload_fetch(&mut self, out: &mut [u8; 2]) -> Result<(), Self::Error>;
    /// Publish one setup-point state register assignment.
    ///
    /// These operations are retained for the pre-`setup_complete` seal; the
    /// action path exports only billboard bytes.
    fn state_set(&mut self, reg: u32, value: u64) -> Result<(), Self::Error>;
    /// Publish one setup-point monotone state-register candidate.
    fn state_max(&mut self, reg: u32, value: u64) -> Result<(), Self::Error>;
    /// Publish one setup-point reachability marker.
    fn reachable(&mut self, point: u32) -> Result<(), Self::Error>;
    /// Yield at a complete controller-chord boundary.
    fn frame_complete(&mut self, frame_count: u64) -> Result<(), Self::Error>;
}

/// Failure to set up or advance the in-guest Nova core.
#[derive(Debug, Eq, PartialEq)]
pub enum NovaError<E> {
    /// SDK transport or emission failure.
    Channel(E),
    /// QuickNES did not expose the expected memory windows.
    MemoryUnavailable,
    /// QuickNES failed to serialize into the billboard.
    SerializeFailed,
    /// Billboard construction failed.
    Billboard(BillboardError),
    /// Fixed setup did not reach Nova gameplay.
    SetupFailed(NovaState),
    /// The u32 billboard frame field would wrap.
    FrameOverflow,
}

impl<E: fmt::Debug> fmt::Display for NovaError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Channel(error) => write!(formatter, "Nova SDK channel failed: {error:?}"),
            Self::MemoryUnavailable => write!(formatter, "QuickNES Nova memory is unavailable"),
            Self::SerializeFailed => write!(formatter, "QuickNES Nova serialization failed"),
            Self::Billboard(error) => write!(formatter, "Nova billboard failed: {error}"),
            Self::SetupFailed(state) => write!(
                formatter,
                "Nova setup did not reach level 1 gameplay: {state:?}"
            ),
            Self::FrameOverflow => write!(formatter, "Nova billboard frame counter overflowed"),
        }
    }
}

impl<E: fmt::Debug> std::error::Error for NovaError<E> {}

/// Run Nova's deterministic title/menu/pre-level controller prefix.
pub fn run_setup<C: Core>(core: &mut C) -> Result<NovaState, NovaError<core::convert::Infallible>> {
    for &(buttons, frames) in BOOT_ACTIONS {
        for _ in 0..frames {
            core.run_frame(buttons);
        }
    }
    let state = read_state(core)?;
    if state.health == 0 || state.x == 0 || state.y == 0 || state.started_level != 0 {
        return Err(NovaError::SetupFailed(state));
    }
    Ok(state)
}

/// QuickNES-backed Nova loop whose snapshots are owned by Consonance.
pub struct NovaAgent<C: Core> {
    core: C,
    layout: NovaBillboardLayout,
    frame_count: u64,
    genesis: NovaState,
    clear_fired: bool,
    collectible_fired: bool,
    ability_fired: bool,
}

impl<C: Core> NovaAgent<C> {
    /// Freeze the billboard layout after setup and capture the genesis state.
    pub fn new(mut core: C) -> Result<Self, NovaError<core::convert::Infallible>> {
        let genesis = read_state(&mut core)?;
        let layout =
            NovaBillboardLayout::new(core.serialize_size()).map_err(NovaError::Billboard)?;
        Ok(Self {
            core,
            layout,
            frame_count: 0,
            genesis,
            clear_fired: false,
            collectible_fired: false,
            ability_fired: false,
        })
    }

    /// Frozen billboard layout for the guest-physical publication.
    #[must_use]
    pub fn layout(&self) -> NovaBillboardLayout {
        self.layout
    }

    /// Cumulative emulated frames after the setup boundary.
    #[must_use]
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// Prime the billboard at the exact setup boundary before sealing it.
    pub fn prime_billboard(
        &mut self,
        billboard: &mut [u8],
    ) -> Result<NovaState, NovaError<core::convert::Infallible>> {
        if billboard.len() < self.layout.total_len() {
            return Err(NovaError::Billboard(BillboardError::BufferTooSmall {
                got: billboard.len(),
                need: self.layout.total_len(),
            }));
        }
        // No action frames exist at setup, so slot zero carries the endpoint
        // work RAM that lets the host decode the sealed genesis uniformly.
        if !self
            .core
            .read_work_ram(self.layout.work_ram_slot_mut(billboard, 0))
        {
            return Err(NovaError::MemoryUnavailable);
        }
        let state = read_state(&mut self.core)?;
        self.publish_final(billboard, 0, 0, state)?;
        Ok(state)
    }

    /// Fetch and execute one chord, publish its raw frame observations, then
    /// yield. The payload and completion are the only SDK calls in this path;
    /// state decoding belongs to the host adapter.
    pub fn run_chord<H: NovaChannel>(
        &mut self,
        channel: &mut H,
        billboard: &mut [u8],
    ) -> Result<NovaState, NovaError<H::Error>> {
        if billboard.len() < self.layout.total_len() {
            return Err(NovaError::Billboard(BillboardError::BufferTooSmall {
                got: billboard.len(),
                need: self.layout.total_len(),
            }));
        }
        let mut payload = [0_u8; 2];
        channel
            .payload_fetch(&mut payload)
            .map_err(NovaError::Channel)?;
        let hold = payload[1].clamp(1, MAX_HOLD_FRAMES);
        let mut frames_run = 0_u8;
        for slot in 0..usize::from(hold) {
            self.core.run_frame(payload[0]);
            self.frame_count = self.frame_count.saturating_add(1);
            let work_ram = self.layout.work_ram_slot_mut(billboard, slot);
            if !self.core.read_work_ram(work_ram) {
                return Err(NovaError::MemoryUnavailable);
            }
            frames_run = frames_run.saturating_add(1);
        }
        // A chord is an exact held-input interval. Decode only its endpoint;
        // death and clear are host-adapter meanings, not guest-side stops.
        let state = read_state(&mut self.core).map_err(widen_error)?;
        // The host sees a coherent endpoint only after the raw ring, save RAM,
        // and savestate are all complete. This is the sole billboard write
        // after the per-frame ring copies.
        self.publish_final(billboard, payload[0], frames_run, state)
            .map_err(widen_error)?;
        channel
            .frame_complete(self.frame_count)
            .map_err(NovaError::Channel)?;
        Ok(state)
    }

    fn publish_final(
        &mut self,
        billboard: &mut [u8],
        joypad: u8,
        frames_run: u8,
        state: NovaState,
    ) -> Result<(), NovaError<core::convert::Infallible>> {
        if billboard.len() < self.layout.total_len() {
            return Err(NovaError::Billboard(BillboardError::BufferTooSmall {
                got: billboard.len(),
                need: self.layout.total_len(),
            }));
        }
        let frame = u32::try_from(self.frame_count).map_err(|_| NovaError::FrameOverflow)?;
        if usize::from(frames_run) > usize::from(MAX_HOLD_FRAMES) {
            // `frames_run` is produced by the bounded loop above. Keeping this
            // check at the serialization boundary makes the wire invariant
            // explicit if another caller is added later.
            return Err(NovaError::FrameOverflow);
        }
        let mut save_ram = [0_u8; NOVA_SAVE_RAM_LEN];
        if self.core.read_save_ram(&mut save_ram) != Some(NOVA_SAVE_RAM_LEN) {
            return Err(NovaError::MemoryUnavailable);
        }
        self.layout
            .save_ram_mut(billboard)
            .copy_from_slice(&save_ram);
        if !self.core.serialize(self.layout.savestate_mut(billboard)) {
            return Err(NovaError::SerializeFailed);
        }
        self.layout
            .write_header(
                billboard,
                frame,
                joypad,
                frames_run,
                state.dead(),
                state.cleared > self.genesis.cleared,
            )
            .map_err(NovaError::Billboard)
    }

    /// Publish the setup-point state through the legacy SDK register catalog.
    ///
    /// The action path deliberately never calls this method: after
    /// `setup_complete`, Nova exports raw memory only and the host adapter
    /// performs all observation decoding.
    pub fn emit_state<H: NovaChannel>(
        &mut self,
        channel: &mut H,
        state: NovaState,
    ) -> Result<(), NovaError<H::Error>> {
        for (reg, value) in [
            (regs::REG_STARTED_LEVEL, u64::from(state.started_level)),
            (regs::REG_LEVEL, u64::from(state.level)),
            (regs::REG_X_BUCKET, u64::from(state.x / 32)),
            (regs::REG_Y_BUCKET, u64::from(state.y / 32)),
            (regs::REG_HEALTH, u64::from(state.health)),
            (regs::REG_ABILITY, u64::from(state.ability)),
            (regs::REG_FRAME, self.frame_count),
            (regs::REG_X, u64::from(state.x)),
            (regs::REG_Y, u64::from(state.y)),
            (regs::REG_CHIPS, u64::from(state.chips)),
            (regs::REG_CHIPS_NEEDED, u64::from(state.chips_needed)),
            (regs::REG_LEVEL_RELOAD, 0),
        ] {
            channel.state_set(reg, value).map_err(NovaError::Channel)?;
        }
        for (reg, value) in [
            (regs::REG_CLEARED, state.cleared),
            (regs::REG_AVAILABLE, state.available),
            (regs::REG_COLLECTIBLES, state.collectibles),
        ] {
            channel
                .state_max(reg, u64::from(value))
                .map_err(NovaError::Channel)?;
        }
        if state.cleared > self.genesis.cleared && !self.clear_fired {
            channel
                .reachable(regs::POINT_LEVEL_CLEARED)
                .map_err(NovaError::Channel)?;
            self.clear_fired = true;
        }
        if state.collectibles > self.genesis.collectibles && !self.collectible_fired {
            channel
                .reachable(regs::POINT_COLLECTIBLE)
                .map_err(NovaError::Channel)?;
            self.collectible_fired = true;
        }
        if state.ability != self.genesis.ability && !self.ability_fired {
            channel
                .reachable(regs::POINT_ABILITY)
                .map_err(NovaError::Channel)?;
            self.ability_fired = true;
        }
        Ok(())
    }
}

fn widen_error<E>(error: NovaError<core::convert::Infallible>) -> NovaError<E> {
    match error {
        NovaError::Channel(never) => match never {},
        NovaError::MemoryUnavailable => NovaError::MemoryUnavailable,
        NovaError::SerializeFailed => NovaError::SerializeFailed,
        NovaError::Billboard(error) => NovaError::Billboard(error),
        NovaError::SetupFailed(state) => NovaError::SetupFailed(state),
        NovaError::FrameOverflow => NovaError::FrameOverflow,
    }
}

fn read_state<C: Core>(core: &mut C) -> Result<NovaState, NovaError<core::convert::Infallible>> {
    let mut wram = [0_u8; WORK_RAM_LEN];
    let mut save_ram = [0_u8; SAVE_RAM_LEN];
    if !core.read_work_ram(&mut wram) || core.read_save_ram(&mut save_ram) != Some(SAVE_RAM_LEN) {
        return Err(NovaError::MemoryUnavailable);
    }
    Ok(decode_state(&wram, &save_ram))
}

fn decode_state(wram: &[u8; WORK_RAM_LEN], save_ram: &[u8; SAVE_RAM_LEN]) -> NovaState {
    NovaState {
        level: wram[LEVEL_NUMBER],
        started_level: wram[STARTED_LEVEL_NUMBER],
        x: u16::from(wram[PLAYER_X_HIGH]) * 16 + u16::from(wram[PLAYER_X_LOW] >> 4),
        y: u16::from(wram[PLAYER_Y_HIGH]) * 16 + u16::from(wram[PLAYER_Y_LOW] >> 4),
        health: wram[PLAYER_HEALTH],
        chips: wram[CHIP_COUNT],
        chips_needed: wram[CHIPS_NEEDED],
        ability: save_ram[PLAYER_ABILITY],
        cleared: bitmap_count(&save_ram[LEVEL_CLEARED..LEVEL_CLEARED + BITMAP_LEN]),
        available: bitmap_count(&save_ram[LEVEL_AVAILABLE..LEVEL_AVAILABLE + BITMAP_LEN]),
        collectibles: bitmap_count(&save_ram[COLLECTIBLE_BITS..COLLECTIBLE_BITS + BITMAP_LEN]),
    }
}

fn bitmap_count(bytes: &[u8]) -> u8 {
    bytes
        .iter()
        .map(|byte| byte.count_ones())
        .sum::<u32>()
        .try_into()
        .unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_seam::MockCore;

    #[derive(Default)]
    struct Tape {
        payload: Option<[u8; 2]>,
        sets: Vec<(u32, u64)>,
        maxes: Vec<(u32, u64)>,
        reachables: Vec<u32>,
        calls: Vec<&'static str>,
        frames: Vec<u64>,
    }

    impl NovaChannel for Tape {
        type Error = &'static str;

        fn payload_fetch(&mut self, out: &mut [u8; 2]) -> Result<(), Self::Error> {
            self.calls.push("payload_fetch");
            *out = self.payload.take().ok_or("payload exhausted")?;
            Ok(())
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

        fn frame_complete(&mut self, frame_count: u64) -> Result<(), Self::Error> {
            self.calls.push("frame_complete");
            self.frames.push(frame_count);
            Ok(())
        }
    }

    fn gameplay_core() -> MockCore {
        let mut core = MockCore::new();
        let ram = core.ram_mut();
        ram[PLAYER_X_HIGH] = 2;
        ram[PLAYER_X_LOW] = 0x60;
        ram[PLAYER_Y_HIGH] = 11;
        ram[PLAYER_Y_LOW] = 0x80;
        ram[PLAYER_HEALTH] = 4;
        core.save_ram_mut()[LEVEL_AVAILABLE] = 1;
        core
    }

    #[test]
    fn one_payload_maps_to_one_consonance_lifecycle_boundary() {
        let mut agent = NovaAgent::new(gameplay_core()).expect("agent");
        let mut billboard = vec![0_u8; agent.layout().total_len()];
        let primed = agent.prime_billboard(&mut billboard).expect("prime");
        assert_eq!((primed.x, primed.y, primed.health), (38, 184, 4));
        assert_eq!(billboard[13], 0);
        assert_eq!(
            agent.layout().work_ram_slot_mut(&mut billboard, 0)[PLAYER_HEALTH],
            4
        );

        let mut tape = Tape {
            payload: Some([0, 3]),
            ..Tape::default()
        };
        let endpoint = agent.run_chord(&mut tape, &mut billboard).expect("chord");
        assert_eq!(endpoint, primed);
        assert_eq!(agent.frame_count(), 3);
        assert_eq!(tape.frames, [3]);
        assert_eq!(tape.calls, ["payload_fetch", "frame_complete"]);
        assert!(tape.sets.is_empty());
        assert!(tape.maxes.is_empty());
        assert!(tape.reachables.is_empty());
        assert_eq!(&billboard[8..12], &3_u32.to_le_bytes());
        assert_eq!(billboard[12], 0);
        assert_eq!(billboard[13], 3);
    }

    struct RingCore {
        inner: MockCore,
        serializes: usize,
        save_reads: usize,
    }

    impl RingCore {
        fn new() -> Self {
            Self {
                inner: gameplay_core(),
                serializes: 0,
                save_reads: 0,
            }
        }
    }

    #[derive(Clone, Copy)]
    enum TerminalCondition {
        Dead,
        Cleared,
    }

    struct TerminalCore {
        inner: RingCore,
        condition: TerminalCondition,
    }

    impl TerminalCore {
        fn new(condition: TerminalCondition) -> Self {
            Self {
                inner: RingCore::new(),
                condition,
            }
        }
    }

    impl Core for TerminalCore {
        fn serialize_size(&mut self) -> usize {
            self.inner.serialize_size()
        }

        fn serialize(&mut self, out: &mut [u8]) -> bool {
            self.inner.serialize(out)
        }

        fn run_frame(&mut self, joypad: u8) {
            self.inner.run_frame(joypad);
            if self.inner.inner.frames_run() == 1 {
                match self.condition {
                    TerminalCondition::Dead => self.inner.inner.ram_mut()[PLAYER_HEALTH] = 0,
                    TerminalCondition::Cleared => {
                        self.inner.inner.save_ram_mut()[LEVEL_CLEARED] = 1;
                    }
                }
            }
        }

        fn read_work_ram(&mut self, out: &mut [u8]) -> bool {
            self.inner.read_work_ram(out)
        }

        fn read_save_ram(&mut self, out: &mut [u8]) -> Option<usize> {
            self.inner.read_save_ram(out)
        }
    }

    impl Core for RingCore {
        fn serialize_size(&mut self) -> usize {
            self.inner.serialize_size()
        }

        fn serialize(&mut self, out: &mut [u8]) -> bool {
            self.serializes += 1;
            self.inner.serialize(out)
        }

        fn run_frame(&mut self, joypad: u8) {
            self.inner.run_frame(joypad);
            self.inner.ram_mut()[0] = self.inner.frames_run() as u8;
        }

        fn read_work_ram(&mut self, out: &mut [u8]) -> bool {
            self.inner.read_work_ram(out)
        }

        fn read_save_ram(&mut self, out: &mut [u8]) -> Option<usize> {
            self.save_reads += 1;
            self.inner.read_save_ram(out)
        }
    }

    #[test]
    fn action_publishes_each_post_frame_ram_slot_and_end_regions_once() {
        let mut core = RingCore::new();
        core.inner.save_ram_mut()[PLAYER_ABILITY] = 0xA5;
        let mut agent = NovaAgent::new(core).expect("agent");
        let mut billboard = vec![0_u8; agent.layout().total_len()];
        let mut tape = Tape {
            payload: Some([0, 3]),
            ..Tape::default()
        };

        agent.run_chord(&mut tape, &mut billboard).expect("chord");

        assert_eq!(tape.calls, ["payload_fetch", "frame_complete"]);
        assert_eq!(billboard[13], 3);
        for slot in 0..3 {
            let ram = agent.layout().work_ram_slot_mut(&mut billboard, slot);
            assert_eq!(ram[0], (slot + 1) as u8);
        }
        assert!(
            agent
                .layout()
                .work_ram_slot_mut(&mut billboard, 3)
                .iter()
                .all(|byte| *byte == 0)
        );
        assert_eq!(
            &agent.layout().save_ram_mut(&mut billboard)[PLAYER_ABILITY..=PLAYER_ABILITY],
            &[0xA5]
        );
        assert_eq!(agent.core.serializes, 1);
        // The state decoder reads the core's save RAM at setup and once at the
        // action endpoint; the billboard window itself is populated only by
        // the single endpoint copy above.
        assert_eq!(agent.core.save_reads, 3);
    }

    fn assert_full_hold_after_terminal(condition: TerminalCondition) {
        let mut agent = NovaAgent::new(TerminalCore::new(condition)).expect("agent");
        let mut billboard = vec![0_u8; agent.layout().total_len()];
        let mut tape = Tape {
            payload: Some([0, 4]),
            ..Tape::default()
        };

        let endpoint = agent.run_chord(&mut tape, &mut billboard).expect("chord");

        assert_eq!(agent.frame_count(), 4);
        assert_eq!(agent.core.inner.inner.frames_run(), 4);
        assert_eq!(billboard[13], 4);
        for slot in 0..4 {
            let ram = agent.layout().work_ram_slot_mut(&mut billboard, slot);
            assert_eq!(ram[0], (slot + 1) as u8);
        }
        assert_eq!(tape.calls, ["payload_fetch", "frame_complete"]);
        assert_eq!(tape.frames, [4]);
        assert_eq!(
            NovaBillboardLayout::dead_flag(&billboard),
            matches!(condition, TerminalCondition::Dead)
        );
        assert_eq!(
            NovaBillboardLayout::cleared_flag(&billboard),
            matches!(condition, TerminalCondition::Cleared)
        );
        assert_eq!(
            endpoint.dead(),
            matches!(condition, TerminalCondition::Dead)
        );
        assert_eq!(
            endpoint.cleared > agent.genesis.cleared,
            matches!(condition, TerminalCondition::Cleared)
        );
    }

    #[test]
    fn dead_core_still_runs_and_publishes_the_full_hold() {
        assert_full_hold_after_terminal(TerminalCondition::Dead);
    }

    #[test]
    fn cleared_core_still_runs_and_publishes_the_full_hold() {
        assert_full_hold_after_terminal(TerminalCondition::Cleared);
    }

    #[test]
    fn register_catalog_fits_the_host_feature_packing_bound() {
        for reg in 1..=regs::REG_LEVEL_RELOAD {
            assert!(reg < (1 << 16));
        }
    }
}
