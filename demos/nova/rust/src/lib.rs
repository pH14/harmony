// SPDX-License-Identifier: AGPL-3.0-or-later

use searcher::search::{
    archive::{Archive, ArchiveCandidate, ArchiveKey},
    rand::RomuDuoJrRand,
};
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, collections::BTreeMap};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(
    inline_js = "export function step(b,n){globalThis.harmonyEngine.run(b,n)} export function capture(){return globalThis.harmonyEngine.capture()} export function restore(s){globalThis.harmonyEngine.restore(s)} export function memory(){return globalThis.harmonyEngine.memory()} "
)]
extern "C" {
    #[wasm_bindgen(catch)]
    fn step(buttons: u8, frames: u8) -> Result<(), JsValue>;
    #[wasm_bindgen(catch)]
    fn capture() -> Result<Vec<u8>, JsValue>;
    #[wasm_bindgen(catch)]
    fn restore(bytes: &[u8]) -> Result<(), JsValue>;
    #[wasm_bindgen(catch)]
    fn memory() -> Result<Vec<u8>, JsValue>;
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Action {
    pub buttons: u8,
    pub frames: u8,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Observation {
    pub x: u16,
    pub y: u16,
    pub health: u8,
    pub level: u8,
    pub selected_level: u8,
    pub checkpoint_level: u8,
    pub program_bank: u8,
    pub chips: u8,
    pub chips_needed: u8,
    pub cleared_levels: [u8; 5],
    pub available_levels: [u8; 5],
    pub ability: u8,
    pub cleared: u8,
    pub available: u8,
    pub collectibles: u8,
    pub reload: bool,
}

impl ArchiveKey for Observation {
    type Place = (u8, u8, u8, u8, u8, bool, u16, u16);
    type Progress = ();
    type Identity = (u16, u16);
    type Lineage = ();
    fn place(self) -> Self::Place {
        (
            self.cleared,
            self.collectibles,
            self.available,
            self.selected_level,
            self.level,
            self.reload,
            self.x / 32,
            self.y / 32,
        )
    }
    fn progress(self) -> Self::Progress {}
    fn identity(self) -> Self::Identity {
        (self.x / 16, self.y / 16)
    }
    fn capacity() -> usize {
        1
    }
    fn preferences() -> usize {
        1
    }
    fn preference_cmp(self, _: usize, other: Self) -> Ordering {
        (
            self.cleared,
            self.collectibles,
            self.available,
            self.ability != 0,
            self.health,
            self.chips,
        )
            .cmp(&(
                other.cleared,
                other.collectibles,
                other.available,
                other.ability != 0,
                other.health,
                other.chips,
            ))
    }
    fn complete(self, _: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }
    fn record(_: &mut Self::Lineage, _: Self) {}
}

pub fn decode(ram: &[u8]) -> Result<Observation, &'static str> {
    if ram.len() != 0x2800 {
        return Err("emulator memory must contain 2 KiB work RAM and 8 KiB save RAM");
    }
    let count = |start: usize| {
        ram[start..start + 8]
            .iter()
            .map(|v| v.count_ones() as u8)
            .sum()
    };
    Ok(Observation {
        x: u16::from(ram[0x26]) * 16 + u16::from(ram[0x25]) / 16,
        y: u16::from(ram[0x27]) * 16 + u16::from(ram[0x28]) / 16,
        health: ram[0x4b],
        level: ram[0xa7],
        selected_level: ram[0xa8],
        checkpoint_level: ram[0x1a59],
        program_bank: ram[0x39e],
        chips: ram[0x508],
        chips_needed: ram[0x509],
        cleared_levels: ram[0x271f..0x2724].try_into().unwrap(),
        available_levels: ram[0x2727..0x272c].try_into().unwrap(),
        ability: ram[0x800 + 0x1200],
        cleared: count(0x800 + 0x1f1f),
        available: count(0x800 + 0x1f27),
        collectibles: count(0x800 + 0x1f2f),
        reload: ram[0xa9] != 0,
    })
}

fn admissible(observation: Observation) -> bool {
    observation.health != 0
}

fn validate_actions(actions: &[Action]) -> Result<(), &'static str> {
    if actions.len() > 10000 {
        return Err("History too long");
    }
    let mut frames = 0_u32;
    for a in actions {
        if a.frames == 0
            || a.frames > 120
            || a.buttons & 12 != 0
            || a.buttons & 48 == 48
            || a.buttons & 192 == 192
        {
            return Err("Invalid controller action");
        }
        frames += u32::from(a.frames);
    }
    if frames > 200000 {
        return Err("History too long");
    }
    Ok(())
}

const DIRECTIONS: [u8; 9] = [0, 0x80, 0x40, 0x10, 0x20, 0x90, 0xa0, 0x50, 0x60];
fn below(rng: &mut RomuDuoJrRand, n: usize) -> usize {
    rng.below(std::num::NonZeroUsize::new(n).unwrap())
}
fn draw(rng: &mut RomuDuoJrRand, previous: Option<Action>) -> Action {
    let buttons = match previous {
        None => DIRECTIONS[below(rng, 9)] | below(rng, 4) as u8,
        Some(p) => match below(rng, 4) {
            0 => {
                let dirs: Vec<_> = DIRECTIONS
                    .iter()
                    .copied()
                    .filter(|d| *d != p.buttons & 0xf0)
                    .collect();
                (p.buttons & 3) | dirs[below(rng, dirs.len())]
            }
            1 => p.buttons ^ 1,
            2 => p.buttons ^ 2,
            _ => p.buttons,
        },
    };
    let frames = if below(rng, 2) == 0 {
        2 + below(rng, 11)
    } else {
        48 + below(rng, 73)
    };
    Action {
        buttons,
        frames: frames as u8,
    }
}

#[derive(Serialize)]
struct Point {
    observation: Observation,
    retained: Option<usize>,
    frame: u32,
}
#[derive(Serialize)]
struct Batch {
    executions: u32,
    states: usize,
    deaths: u32,
    frames: u64,
    snapshot_bytes: usize,
    stopped: bool,
    won: bool,
    points: Vec<Point>,
}
#[derive(Serialize)]
struct State {
    id: usize,
    observation: Observation,
    actions: Vec<Action>,
    frames: u32,
}

#[wasm_bindgen]
pub struct Explorer {
    archive: Archive<Action, Observation, (), ()>,
    snapshots: BTreeMap<usize, Box<[u8]>>,
    digests: BTreeMap<usize, u64>,
    rng: RomuDuoJrRand,
    executions: u32,
    deaths: u32,
    frames: u64,
    stopped: bool,
    won: bool,
    snapshot_bytes: usize,
    max_snapshot_bytes: usize,
    retired_snapshots: u32,
    prefix: Vec<Action>,
}

const MIN_SNAPSHOT_BUDGET: usize = 2 * 1024 * 1024;
const MIN_ACTIVE_ENTRIES: usize = 256;
const MAX_ARCHIVE_HISTORY: usize = 400_000;

fn snapshot_digest(bytes: &[u8]) -> u64 {
    let mut a: u32 = 0x811c_9dc5;
    let mut b: u32 = 0x01c9_3a75;
    for &byte in bytes {
        a = (a ^ u32::from(byte)).wrapping_mul(0x0100_0193);
        b = (b ^ u32::from(byte))
            .wrapping_mul(0x0100_0193)
            .rotate_left(5);
    }
    (u64::from(a) << 32) | u64::from(b)
}

fn compress_snapshot(bytes: &[u8]) -> Box<[u8]> {
    miniz_oxide::deflate::compress_to_vec(bytes, 1).into_boxed_slice()
}

fn js_error(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

#[wasm_bindgen]
impl Explorer {
    #[wasm_bindgen(constructor)]
    pub fn new(seed: u32) -> Result<Explorer, JsValue> {
        Self::root(seed, Vec::new(), true)
    }

    pub fn from_history(seed: u32, history: &str) -> Result<Explorer, JsValue> {
        if history.len() > 1000000 {
            return Err(js_error("History file too large"));
        }
        let prefix: Vec<Action> = serde_json::from_str(history).map_err(js_error)?;
        validate_actions(&prefix).map_err(js_error)?;
        Self::root(seed, prefix, false)
    }

    pub fn state_count(&self) -> usize {
        self.snapshots.len()
    }

    pub fn snapshot_bytes(&self) -> usize {
        self.snapshot_bytes
    }

    fn root(seed: u32, prefix: Vec<Action>, genesis: bool) -> Result<Explorer, JsValue> {
        let obs = decode(&memory()?).map_err(js_error)?;
        if obs.health == 0 || (genesis && (obs.x == 0 || obs.y == 0 || obs.selected_level != 0)) {
            return Err(js_error("Nova setup did not reach level one"));
        }
        let mut archive = Archive::new(|a: &Action| u64::from(a.frames));
        archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: Vec::<Action>::new(),
                    key: obs,
                    milestones: (),
                },
                (),
            )
            .map_err(js_error)?;
        let won = obs.cleared_levels == [255; 5];
        let stopped = won
            || prefix.len() >= 10000
            || prefix.iter().map(|a| u32::from(a.frames)).sum::<u32>() >= 200000;
        let raw = capture()?;
        let root = compress_snapshot(&raw);
        let snapshot_bytes = root.len();
        Ok(Self {
            archive,
            rng: RomuDuoJrRand::with_seed(u64::from(seed)),
            executions: 0,
            deaths: 0,
            frames: 0,
            snapshots: BTreeMap::from([(0, root)]),
            digests: BTreeMap::from([(0, snapshot_digest(&raw))]),
            stopped,
            won,
            snapshot_bytes,
            max_snapshot_bytes: 128 * 1024 * 1024,
            retired_snapshots: 0,
            prefix,
        })
    }

    fn history(&self, id: usize) -> Result<Vec<Action>, JsValue> {
        let mut actions = self.prefix.clone();
        actions.extend(self.archive.entry_input(id).map_err(js_error)?.actions);
        Ok(actions)
    }

    pub fn set_snapshot_budget(&mut self, bytes: u32) {
        self.max_snapshot_bytes = (bytes as usize).clamp(MIN_SNAPSHOT_BUDGET, 128 * 1024 * 1024);
    }

    pub fn retired_snapshots(&self) -> u32 {
        self.retired_snapshots
    }

    fn release_retired_snapshots(&mut self) {
        let active = &self.archive.active;
        let mut freed = 0;
        let mut released = 0;
        self.snapshots.retain(|&id, bytes| {
            let keep = id == 0 || active.get(id).copied().unwrap_or(false);
            if !keep {
                freed += bytes.len();
                released += 1;
            }
            keep
        });
        self.snapshot_bytes -= freed;
        self.retired_snapshots += released;
    }

    fn enforce_snapshot_budget(&mut self) {
        self.release_retired_snapshots();
        if self.snapshot_bytes > self.max_snapshot_bytes {
            let live = self.archive.active_count();
            self.archive.max_entries = live.saturating_sub(live / 64 + 1).max(MIN_ACTIVE_ENTRIES);
        }
    }

    pub fn advance(&mut self, jobs: u32) -> Result<String, JsValue> {
        let mut points = Vec::new();
        for _ in 0..jobs.min(8) {
            if self.stopped {
                break;
            }
            let (parent, selection) = self
                .archive
                .select_parent(&mut self.rng)
                .map_err(js_error)?;
            self.archive.record_selection(parent, &selection);
            restore(&self.snapshot(parent)?)?;
            let prefix = self.history(parent)?;
            let mut frame: u32 = prefix.iter().map(|a| u32::from(a.frames)).sum();
            let mut previous = prefix.last().copied();
            let mut current_parent = parent;
            let mut suffix = Vec::new();
            let mut productive = false;
            let count = 1 + below(&mut self.rng, 8);
            for action_count in (prefix.len()..).take(count) {
                let action = draw(&mut self.rng, previous);
                if frame + u32::from(action.frames) > 200_000 || action_count >= 10_000 {
                    break;
                }
                step(action.buttons, action.frames)?;
                self.frames += u64::from(action.frames);
                frame += u32::from(action.frames);
                suffix.push(action);
                previous = Some(action);
                let obs = decode(&memory()?).map_err(js_error)?;
                if !admissible(obs) {
                    self.deaths += 1;
                    break;
                }
                let retained_before = self.archive.live_entry_count();
                let id = self
                    .archive
                    .insert(
                        Some(current_parent),
                        u64::from(self.executions) + 1,
                        ArchiveCandidate {
                            suffix: suffix.clone(),
                            key: obs,
                            milestones: (),
                        },
                        (),
                    )
                    .map_err(js_error)?;
                if let Some(id) = id
                    && let std::collections::btree_map::Entry::Vacant(entry) =
                        self.snapshots.entry(id)
                {
                    let raw = capture()?;
                    let compressed = compress_snapshot(&raw);
                    self.snapshot_bytes += compressed.len();
                    entry.insert(compressed);
                    self.digests.insert(id, snapshot_digest(&raw));
                }
                productive |= self.archive.live_entry_count() > retained_before;
                if let Some(id) = id {
                    current_parent = id;
                    suffix.clear();
                }
                points.push(Point {
                    observation: obs,
                    retained: id,
                    frame,
                });
            }
            self.archive.record_selection_outcome(parent, productive);
            self.enforce_snapshot_budget();
            self.executions += 1;
            self.won |= points
                .iter()
                .any(|p| p.observation.cleared_levels == [255; 5]);
            self.stopped = self.won
                || self.archive.live_entry_count() >= MAX_ARCHIVE_HISTORY
                || self.executions >= 100000;
        }
        serde_json::to_string(&Batch {
            executions: self.executions,
            states: self.snapshots.len(),
            deaths: self.deaths,
            frames: self.frames,
            snapshot_bytes: self.snapshot_bytes,
            stopped: self.stopped,
            won: self.won,
            points,
        })
        .map_err(js_error)
    }

    pub fn state(&self, id: usize) -> Result<String, JsValue> {
        let observation = self
            .archive
            .entry_key(id)
            .ok_or_else(|| js_error("unknown state"))?;
        let actions = self.history(id)?;
        let frames = actions.iter().map(|a| u32::from(a.frames)).sum();
        serde_json::to_string(&State {
            id,
            observation,
            actions,
            frames,
        })
        .map_err(js_error)
    }

    pub fn digest(&self, id: usize) -> Result<String, JsValue> {
        self.digests
            .get(&id)
            .map(|digest| format!("{digest:016x}"))
            .ok_or_else(|| js_error("digest unavailable"))
    }

    pub fn snapshot(&self, id: usize) -> Result<Vec<u8>, JsValue> {
        let compressed = self
            .snapshots
            .get(&id)
            .ok_or_else(|| js_error("snapshot unavailable"))?;
        miniz_oxide::inflate::decompress_to_vec_with_limit(compressed, 1048576)
            .map_err(|_| js_error("invalid compressed snapshot"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_digest_matches_the_browser_reference() {
        assert_eq!(snapshot_digest(&[]), 0x811c_9dc5_01c9_3a75);
        assert_eq!(snapshot_digest(b"nova"), 0x8193_e2df_c1eb_c4ee);
        assert_ne!(snapshot_digest(b"nova"), snapshot_digest(b"novb"));
    }
    #[test]
    fn recorded_next_level_is_admitted_with_its_real_input_suffix() {
        #[derive(Deserialize)]
        struct Tape {
            observation: Observation,
            actions: Vec<Action>,
        }
        let before: Tape =
            serde_json::from_str(include_str!("../../tests/fixtures/main-exit.json")).unwrap();
        let after: Tape =
            serde_json::from_str(include_str!("../../tests/fixtures/level-two-2.json")).unwrap();
        assert_eq!(before.observation.selected_level, 0);
        assert_eq!(after.observation.selected_level, 1);
        assert_eq!(after.observation.cleared_levels[0] & 1, 1);
        assert!(admissible(before.observation));
        assert!(admissible(after.observation));
        assert!(after.actions.starts_with(&before.actions));
        let suffix = after.actions[before.actions.len()..].to_vec();
        assert!(!suffix.is_empty());
        let mut archive = Archive::new(|a: &Action| u64::from(a.frames));
        let root = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: Vec::<Action>::new(),
                    key: before.observation,
                    milestones: (),
                },
                (),
            )
            .unwrap()
            .unwrap();
        let id = archive
            .insert(
                Some(root),
                1,
                ArchiveCandidate {
                    suffix: suffix.clone(),
                    key: after.observation,
                    milestones: (),
                },
                (),
            )
            .unwrap()
            .unwrap();
        assert_ne!(root, id);
        assert_eq!(archive.entry_input(id).unwrap().actions, suffix);
    }

    #[test]
    fn human_histories_reject_invalid_inputs_and_bound_total_work() {
        assert!(
            validate_actions(&[Action {
                buttons: 128,
                frames: 1
            }])
            .is_ok()
        );
        for action in [
            Action {
                buttons: 8,
                frames: 1,
            },
            Action {
                buttons: 48,
                frames: 1,
            },
            Action {
                buttons: 192,
                frames: 1,
            },
            Action {
                buttons: 0,
                frames: 0,
            },
            Action {
                buttons: 0,
                frames: 121,
            },
        ] {
            assert!(validate_actions(&[action]).is_err());
        }
        assert!(
            validate_actions(&vec![
                Action {
                    buttons: 0,
                    frames: 120
                };
                1667
            ])
            .is_err()
        );
        assert!(
            validate_actions(&vec![
                Action {
                    buttons: 0,
                    frames: 1
                };
                10001
            ])
            .is_err()
        );
    }

    #[test]
    fn nova_addresses_decode_fixed_point_and_save_ram() {
        let mut ram = vec![0; 0x2800];
        ram[0x25] = 0x80;
        ram[0x26] = 3;
        ram[0x27] = 9;
        ram[0x28] = 0x40;
        ram[0x4b] = 4;
        ram[0x1a00] = 2;
        ram[0x271f] = 5;
        ram[0x2723] = 128;
        ram[0x2727] = 3;
        ram[0x508] = 7;
        ram[0x509] = 12;
        ram[0x1a59] = 45;
        ram[0x39e] = 9;
        let state = decode(&ram).unwrap();
        assert_eq!(
            (state.x, state.y, state.health, state.ability, state.cleared),
            (56, 148, 4, 2, 3)
        );
        assert_eq!((state.checkpoint_level, state.program_bank), (45, 9));
        assert_eq!(state.cleared_levels, [5, 0, 0, 0, 128]);
        assert_eq!(state.available_levels, [3, 0, 0, 0, 0]);
        assert_eq!((state.chips, state.chips_needed), (7, 12));
        assert!(decode(&ram[..0x800]).is_err());
    }
    #[test]
    fn compressed_snapshot_owns_only_its_payload_and_round_trips() {
        let bytes: Vec<u8> = (0..20998).map(|i| (i % 256) as u8).collect();
        let compressed: Box<[u8]> = compress_snapshot(&bytes);
        assert!(compressed.len() < bytes.len() / 2);
        assert_eq!(
            miniz_oxide::inflate::decompress_to_vec_with_limit(&compressed, 1048576).unwrap(),
            bytes
        );
    }
    #[test]
    fn heatmap_position_is_not_full_archive_identity() {
        let a = decode(&vec![0; 0x2800]).unwrap();
        let b = Observation { health: 4, ..a };
        assert_eq!(a.place(), b.place());
        let transition = Observation { reload: true, ..b };
        assert_ne!(transition.place(), b.place());
        assert_eq!(b.preference_cmp(0, a), Ordering::Greater);
        let mut rng = RomuDuoJrRand::with_seed(1);
        let mut prev = None;
        for _ in 0..10000 {
            let a = draw(&mut rng, prev);
            assert_eq!(a.buttons & 12, 0);
            assert_ne!(a.buttons & 0x30, 0x30);
            assert_ne!(a.buttons & 0xc0, 0xc0);
            assert!((2..=12).contains(&a.frames) || (48..=120).contains(&a.frames));
            prev = Some(a);
        }
    }
    #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
    struct ProbeKey(u8);
    impl ArchiveKey for ProbeKey {
        type Place = u8;
        type Progress = u8;
        type Identity = ();
        type Lineage = ();
        fn place(self) -> u8 {
            self.0
        }
        fn progress(self) -> u8 {
            self.0
        }
        fn identity(self) {}
        fn tier_rank_shift() -> u32 {
            7
        }
        fn complete(self, _: Option<(Self, &Self::Lineage)>) -> Self {
            self
        }
        fn record(_: &mut Self::Lineage, _: Self) {}
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn wide_selector_matches_recorded_native_choices() {
        let mut archive = Archive::<u8, ProbeKey, (), ()>::new(|_| 1);
        for id in 0..3 {
            archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        suffix: vec![id],
                        key: ProbeKey(id),
                        milestones: (),
                    },
                    (),
                )
                .unwrap();
        }
        let mut rng = RomuDuoJrRand::with_seed(42);
        let choices: Vec<_> = (0..32)
            .map(|_| {
                let (id, draw) = archive.select_parent(&mut rng).unwrap();
                archive.record_selection(id, &draw);
                id
            })
            .collect();
        assert_eq!(
            choices,
            [
                2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 2, 1, 2, 2, 2, 2,
                2, 2, 2, 2
            ]
        );
    }
}
