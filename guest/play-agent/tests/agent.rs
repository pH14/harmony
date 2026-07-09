// SPDX-License-Identifier: AGPL-3.0-or-later
//! Portable integration + property gates for the play-agent brain (task 86
//! gate 1): chord decode from a fixed entropy stream reproduces a fixed input
//! tape; the RAM-map decode is exercised for every register; the billboard
//! header/layout round-trips; all against the mock core — no ROM, no emulator,
//! no FFI anywhere.

use harmony_play_agent::agent::{Agent, AgentConfig, Harness};
use harmony_play_agent::billboard::{BILLBOARD_LAYOUT_VERSION, BILLBOARD_MAGIC, HEADER_LEN};
use harmony_play_agent::chord::ChordAlphabet;
use harmony_play_agent::core_seam::MockCore;
use harmony_play_agent::ram::{self, WORK_RAM_LEN, addr};
use harmony_play_agent::regs;
use proptest::prelude::*;

/// A recording harness with a scripted entropy stream (the portable stand-in
/// for the SDK).
#[derive(Default)]
struct FakeHarness {
    entropy: Vec<u8>,
    cursor: usize,
    sets: Vec<(u32, u64)>,
    maxes: Vec<(u32, u64)>,
    reachables: Vec<u32>,
}

impl FakeHarness {
    fn scripted(entropy: Vec<u8>) -> Self {
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

/// The fixed-tape gate: a fixed entropy stream must reproduce a fixed,
/// hand-checked input tape — the portable proof that the decision path is a
/// pure function of the entropy bytes.
#[test]
fn fixed_entropy_stream_reproduces_the_fixed_input_tape() {
    use harmony_play_agent::chord::joypad::{A, DOWN, RIGHT};
    let cfg = AgentConfig {
        window: 3,
        ..AgentConfig::default()
    };
    let mut agent = Agent::new(MockCore::in_gameplay(), cfg).unwrap();
    // Hand-decoded against the default alphabet's cumulative weights
    // (RIGHT 0..=55, RIGHT+B 56..=111, RIGHT+A 112..=159, RIGHT+A+B 160..=207,
    // A 208..=223, LEFT 224..=235, DOWN 236..=247, neutral 248..=255).
    let mut h = FakeHarness::scripted(vec![0, 112, 240, 255, 208]);
    let mut buf = vec![0u8; agent.layout().total_len()];
    let mut tape = Vec::new();
    for _ in 0..15 {
        tape.push(agent.step(&mut h, &mut buf).unwrap().joypad);
    }
    let expected = [
        RIGHT,
        RIGHT,
        RIGHT,
        RIGHT | A,
        RIGHT | A,
        RIGHT | A,
        DOWN,
        DOWN,
        DOWN,
        0,
        0,
        0,
        A,
        A,
        A,
    ];
    assert_eq!(tape, expected);
}

/// Two agents fed the same entropy stream must agree on every emission — the
/// portable determinism gate.
#[test]
fn identical_entropy_streams_agree_on_every_emission() {
    let run = || {
        let mut agent = Agent::new(MockCore::in_gameplay(), AgentConfig::default()).unwrap();
        let mut h = FakeHarness::scripted((0..=255).collect());
        let mut buf = vec![0u8; agent.layout().total_len()];
        let mut billboards = Vec::new();
        for _ in 0..64 {
            agent.step(&mut h, &mut buf).unwrap();
            billboards.push(buf.clone());
        }
        (h.sets, h.maxes, h.reachables, billboards)
    };
    assert_eq!(run(), run());
}

/// The register emissions carry the planted RAM fixture values through the
/// billboard's work-RAM region (every register, end to end).
#[test]
fn registers_flow_from_planted_ram_to_emissions() {
    let cfg = AgentConfig {
        window: 1,
        x_bucket_px: 128,
        alphabet: ChordAlphabet::smb_default(),
    };
    let mut core = MockCore::new();
    core.ram_mut()[addr::OPER_MODE] = ram::OPER_MODE_GAMEPLAY;
    core.ram_mut()[addr::WORLD_NUMBER] = 2; // World 3
    core.ram_mut()[addr::LEVEL_NUMBER] = 3; // 3-4
    core.ram_mut()[addr::PLAYER_PAGE_LOC] = 4;
    core.ram_mut()[addr::PLAYER_X_POSITION] = 200;
    core.ram_mut()[addr::PLAYER_STATUS] = 1;
    let mut agent = Agent::new(core, cfg).unwrap();
    let mut h = FakeHarness::scripted(vec![255]); // neutral chord: no movement
    let mut buf = vec![0u8; agent.layout().total_len()];
    agent.step(&mut h, &mut buf).unwrap();

    let get = |reg: u32| -> u64 {
        h.sets
            .iter()
            .find(|(r, _)| *r == reg)
            .map(|(_, v)| *v)
            .unwrap_or_else(|| panic!("reg {reg} not emitted"))
    };
    assert_eq!(get(regs::REG_GAME_MODE), 1);
    assert_eq!(get(regs::REG_WORLD), 2);
    assert_eq!(get(regs::REG_LEVEL), 3);
    assert_eq!(get(regs::REG_X_BUCKET), u64::from((4u32 * 256 + 200) / 128));
    assert_eq!(get(regs::REG_POWERUP), 1);
    assert_eq!(get(regs::REG_FRAME), 0);
    assert_eq!(h.maxes, vec![(regs::REG_DEPTH, 2 * 4 + 3)]);
}

/// The billboard round-trips through a parse that mirrors film's validations
/// (magic, version, frame, joypad, region table, bounds) on every frame.
#[test]
fn billboard_round_trips_films_validations_every_frame() {
    let mut agent = Agent::new(MockCore::in_gameplay(), AgentConfig::default()).unwrap();
    let mut h = FakeHarness::scripted((0..64).collect());
    let mut buf = vec![0u8; agent.layout().total_len()];
    for expected_frame in 0..40u32 {
        let report = agent.step(&mut h, &mut buf).unwrap();
        let (frame, joypad, regions) = parse_billboard(&buf).expect("valid billboard");
        assert_eq!(frame, expected_frame);
        assert_eq!(joypad, report.joypad);
        assert_eq!(
            regions,
            (HEADER_LEN, agent.layout().savestate_len(), WORK_RAM_LEN)
        );
    }
}

/// The parsed fields the mirror checks return: the frame, the joypad byte,
/// and the `(savestate_off, savestate_len, workram_len)` region triple.
type ParsedBillboard = (u32, u8, (usize, usize, usize));

/// A mirror of film's `BillboardHeader::parse` checks.
fn parse_billboard(buf: &[u8]) -> Result<ParsedBillboard, String> {
    if buf.len() < HEADER_LEN {
        return Err("too short".into());
    }
    if buf[0..4] != BILLBOARD_MAGIC {
        return Err("bad magic".into());
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().unwrap());
    if version != BILLBOARD_LAYOUT_VERSION {
        return Err("version skew".into());
    }
    let frame = u32::from_le_bytes(buf[8..12].try_into().unwrap());
    let joypad = buf[12];
    let ss_off = u32::from_le_bytes(buf[16..20].try_into().unwrap()) as usize;
    let ss_len = u32::from_le_bytes(buf[20..24].try_into().unwrap()) as usize;
    let wr_off = u32::from_le_bytes(buf[24..28].try_into().unwrap()) as usize;
    let wr_len = u32::from_le_bytes(buf[28..32].try_into().unwrap()) as usize;
    // Film's region validations: regions past the header, inside the buffer,
    // non-overlapping, contiguous as this producer lays them out.
    if ss_off < HEADER_LEN || ss_off + ss_len > buf.len() {
        return Err("savestate out of bounds".into());
    }
    if wr_off < HEADER_LEN || wr_off + wr_len > buf.len() {
        return Err("work_ram out of bounds".into());
    }
    if wr_off != ss_off + ss_len {
        return Err("regions not contiguous".into());
    }
    Ok((frame, joypad, (ss_off, ss_len, wr_len)))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(if cfg!(miri) { 8 } else { 256 }))]

    /// Any entropy stream: every held chord is a member of the alphabet, one
    /// draw happens per window, and the billboard stays valid on every frame.
    #[test]
    fn chords_come_from_the_alphabet_and_billboards_stay_valid(
        entropy in prop::collection::vec(any::<u8>(), 16..64),
        window in 1u32..24,
    ) {
        let alphabet = ChordAlphabet::smb_default();
        let legal: Vec<u8> = alphabet.entries().iter().map(|c| c.buttons).collect();
        let cfg = AgentConfig { window, x_bucket_px: 128, alphabet };
        let mut agent = Agent::new(MockCore::in_gameplay(), cfg).unwrap();
        let mut h = FakeHarness::scripted(entropy.clone());
        let mut buf = vec![0u8; agent.layout().total_len()];
        let frames = u64::from(window) * (entropy.len() as u64);
        for _ in 0..frames.min(200) {
            let report = agent.step(&mut h, &mut buf).unwrap();
            prop_assert!(legal.contains(&report.joypad));
            prop_assert!(parse_billboard(&buf).is_ok());
        }
        // One draw per window, exactly.
        let stepped = frames.min(200);
        prop_assert_eq!(h.cursor as u64, stepped.div_ceil(u64::from(window)));
    }

    /// The x-bucket register divides absolute X by the configured bucket for
    /// any planted position.
    #[test]
    fn x_bucket_divides_absolute_x(page in 0u8..=255, x in 0u8..=255, bucket in 1u32..512) {
        let cfg = AgentConfig { window: 1, x_bucket_px: bucket, alphabet: ChordAlphabet::smb_default() };
        let mut core = MockCore::new();
        core.ram_mut()[addr::OPER_MODE] = ram::OPER_MODE_GAMEPLAY;
        core.ram_mut()[addr::PLAYER_PAGE_LOC] = page;
        core.ram_mut()[addr::PLAYER_X_POSITION] = x;
        let mut agent = Agent::new(core, cfg).unwrap();
        let mut h = FakeHarness::scripted(vec![255]); // neutral: no movement
        let mut buf = vec![0u8; agent.layout().total_len()];
        agent.step(&mut h, &mut buf).unwrap();
        let expected = u64::from((u32::from(page) * 256 + u32::from(x)) / bucket);
        let got = h.sets.iter().find(|(r, _)| *r == regs::REG_X_BUCKET).unwrap().1;
        prop_assert_eq!(got, expected);
    }

    /// Alphabet parse/round-trip: any valid weighted alphabet decodes every
    /// byte to one of its own chords, at the frequency its weights dictate.
    #[test]
    fn alphabet_decode_frequencies_match_weights(
        weights in prop::collection::vec(1u16..=64, 2..8),
    ) {
        // Normalize the last weight so the sum is exactly 256 (reject if
        // impossible for this draw).
        let partial: u16 = weights[..weights.len() - 1].iter().sum();
        prop_assume!(partial < 256 && (256 - partial) >= 1);
        let mut entries: Vec<harmony_play_agent::chord::Chord> = weights
            .iter()
            .enumerate()
            .map(|(i, w)| harmony_play_agent::chord::Chord { buttons: i as u8, weight: *w })
            .collect();
        let last = entries.len() - 1;
        entries[last].weight = 256 - partial;
        let alphabet = ChordAlphabet::new(entries.clone()).unwrap();
        let mut counts = vec![0u32; entries.len()];
        for byte in 0..=255u8 {
            let chord = alphabet.decode(byte);
            // `buttons` is the entry index here, so the position is unique.
            let idx = entries.iter().position(|c| c.buttons == chord).unwrap();
            counts[idx] += 1;
        }
        for (i, entry) in entries.iter().enumerate() {
            prop_assert_eq!(counts[i], u32::from(entry.weight));
        }
    }
}
