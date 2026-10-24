// SPDX-License-Identifier: AGPL-3.0-or-later

mod campaign;
mod famidash;
mod mega_man;
mod metroid;
mod punchout;
mod smb;
mod twin_dragons;

use nes_games::arcade::snapshot_digest;
use searcher::search::{
    archive::{Archive, ArchiveCandidate, ArchiveKey},
    rand::RomuDuoJrRand,
};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, VecDeque},
};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(
    inline_js = "export function capture(){return globalThis.harmonyEngine.capture()} export function memory(){return globalThis.harmonyEngine.memory()} "
)]
extern "C" {
    #[wasm_bindgen(catch)]
    fn capture() -> Result<Vec<u8>, JsValue>;
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
    pub level: u16,
    pub gamestate: u8,
    pub goal: bool,
    pub loading: bool,
    pub cleared: u8,
    pub scene: u8,
    pub climb: u16,
    pub time: u8,
}

const LEVEL_COUNT: u8 = 3;
const TOBU_RAM_BYTES: usize = 0x400;
const TOBU_IN_LEVEL: u8 = 4;
const TOBU_ENDING: u8 = 10;
const TOBU_LEVEL_COMPLETE: u8 = 2;
const TOBU_BAND: u16 = 1024;
const TOBU_MAP_HEIGHTS: [u16; 3] = [2000, 2896, 3792];
const TOBU_SPRITE_LEFT: u16 = 24;
const TOBU_SPRITE_CENTER: u16 = 155;
fn admissible(o: Observation) -> bool {
    !o.loading || o.gamestate == TOBU_ENDING
}

fn level_complete(o: Observation) -> bool {
    o.gamestate == TOBU_IN_LEVEL && o.scene == TOBU_LEVEL_COMPLETE
}

fn validate_actions(actions: &[Action]) -> Result<(), &'static str> {
    if actions.len() > 10000 {
        return Err("History too long");
    }
    let mut frames = 0_u32;
    for a in actions {
        if a.frames == 0 || a.buttons & 48 == 48 || a.buttons & 192 == 192 {
            return Err("Invalid controller action");
        }
        frames += u32::from(a.frames);
    }
    if frames > 200000 {
        return Err("History too long");
    }
    Ok(())
}

impl ArchiveKey for Observation {
    type Place = (u16, bool, u16, u16);
    type Progress = (u8, u16);
    type Identity = (u16, u16);
    type Lineage = ();
    fn place(self) -> Self::Place {
        (
            self.level,
            self.loading,
            self.climb / 32,
            (self.x + TOBU_SPRITE_LEFT) / 32,
        )
    }
    fn progress(self) -> Self::Progress {
        (self.cleared, self.climb / TOBU_BAND)
    }
    fn identity(self) -> Self::Identity {
        (0, 0)
    }
    fn capacity() -> usize {
        1
    }
    fn preferences() -> usize {
        1
    }
    fn preference_cmp(self, _: usize, other: Self) -> Ordering {
        (self.cleared, self.time, self.climb).cmp(&(other.cleared, other.time, other.climb))
    }
    fn complete(self, _: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }
    fn record(_: &mut Self::Lineage, _: Self) {}
}

impl Observation {
    fn won(self) -> bool {
        self.gamestate == TOBU_ENDING
    }
}

pub fn decode_tobu(ram: &[u8]) -> Result<Observation, &'static str> {
    if ram.len() != TOBU_RAM_BYTES {
        return Err("emulator memory must contain 1 KiB of work RAM bank 0");
    }
    let gamestate = ram[0xa5];
    let scene = ram[0xa6];
    let level = ram[0xa7].saturating_sub(1);
    let in_level = gamestate == TOBU_IN_LEVEL && scene == 0;
    let climb = u16::from(ram[0x233]) * u16::from(ram[0x237]) + u16::from(ram[0x230]);
    let cleared = if gamestate == TOBU_ENDING {
        LEVEL_COUNT
    } else {
        level
    };
    Ok(Observation {
        x: u16::from(ram[0xc2]).saturating_sub(TOBU_SPRITE_LEFT),
        y: (TOBU_MAP_HEIGHTS[usize::from(level.min(2))] + u16::from(ram[0xc3]))
            .saturating_sub(TOBU_SPRITE_CENTER + climb),
        health: u8::from(in_level || gamestate == TOBU_ENDING),
        level: u16::from(level),
        gamestate,
        goal: ram[0x235] != 0,
        loading: !in_level,
        cleared,
        scene,
        climb,
        time: ram[0xb0],
    })
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
        6 + below(rng, 13)
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
    pending: VecDeque<Job>,
    points: Vec<Point>,
    snapshot_size: usize,
}

struct Job {
    parent: usize,
    actions: Vec<Action>,
    frame: u32,
}

const MIN_SNAPSHOT_BUDGET: usize = 2 * 1024 * 1024;
const MIN_ACTIVE_ENTRIES: usize = 256;

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
        let obs = decode_tobu(&memory()?).map_err(js_error)?;
        if !admissible(obs) || (genesis && (obs.loading || obs.level != 0)) {
            return Err(js_error("Game setup did not reach level one"));
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
        let won = obs.won();
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
            pending: VecDeque::new(),
            points: Vec::new(),
            snapshot_size: raw.len(),
        })
    }

    fn history(&self, id: usize) -> Result<Vec<Action>, JsValue> {
        let mut actions = self.prefix.clone();
        actions.extend(self.archive.entry_input(id).map_err(js_error)?.actions);
        Ok(actions)
    }

    pub fn set_snapshot_budget(&mut self, bytes: u32) {
        self.max_snapshot_bytes = (bytes as usize).max(MIN_SNAPSHOT_BUDGET);
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

    pub fn plan(&mut self, jobs: u32) -> Result<Vec<u32>, JsValue> {
        let mut plan = Vec::new();
        for _ in 0..jobs {
            if self.stopped {
                break;
            }
            let (parent, selection) = self
                .archive
                .select_parent(&mut self.rng)
                .map_err(js_error)?;
            self.archive.record_selection(parent, &selection);
            let prefix = self.history(parent)?;
            let start: u32 = prefix.iter().map(|a| u32::from(a.frames)).sum();
            let mut frame = start;
            let mut previous = prefix.last().copied();
            let mut actions = Vec::new();
            let count = 1 + below(&mut self.rng, 24);
            for action_count in (prefix.len()..).take(count) {
                let action = draw(&mut self.rng, previous);
                if frame + u32::from(action.frames) > 200_000 || action_count >= 10_000 {
                    break;
                }
                frame += u32::from(action.frames);
                actions.push(action);
                previous = Some(action);
            }
            plan.push(u32::try_from(parent).map_err(js_error)?);
            plan.push(actions.len() as u32);
            plan.extend(
                actions
                    .iter()
                    .map(|a| (u32::from(a.buttons) << 8) | u32::from(a.frames)),
            );
            self.pending.push_back(Job {
                parent,
                actions,
                frame: start,
            });
        }
        Ok(plan)
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    pub fn stopped(&self) -> bool {
        self.stopped
    }

    pub fn merge(&mut self, steps: &[u8], transition: &[u32]) -> Result<(), JsValue> {
        if self.stopped {
            return Ok(());
        }
        let job = self
            .pending
            .pop_front()
            .ok_or_else(|| js_error("no rollout is pending"))?;
        let ram_bytes = TOBU_RAM_BYTES;
        let stride = ram_bytes + self.snapshot_size;
        let transition: Vec<Action> = transition
            .iter()
            .map(|&packed| Action {
                buttons: (packed >> 8) as u8,
                frames: packed as u8,
            })
            .collect();
        let planned = steps.len() / stride - usize::from(!transition.is_empty());
        if !steps.len().is_multiple_of(stride)
            || steps.len() < stride * usize::from(!transition.is_empty())
            || planned > job.actions.len()
            || transition.iter().any(|a| a.frames == 0)
        {
            return Err(js_error("rollout result does not match its plan"));
        }
        let mut frame = job.frame;
        let mut current_parent = job.parent;
        let mut suffix = Vec::new();
        let mut productive = false;
        let mut ended = false;
        let mut taken: Vec<(Vec<Action>, &[u8])> = job.actions[..planned]
            .iter()
            .zip(steps.chunks_exact(stride))
            .map(|(action, step)| (vec![*action], step))
            .collect();
        if !transition.is_empty() {
            taken.push((transition.clone(), &steps[planned * stride..]));
        }
        let last = taken.len().saturating_sub(1);
        for (index, (actions, step)) in taken.into_iter().enumerate() {
            let (ram, raw) = step.split_at(ram_bytes);
            for action in &actions {
                self.frames += u64::from(action.frames);
                frame += u32::from(action.frames);
            }
            suffix.extend(actions);
            let obs = decode_tobu(ram).map_err(js_error)?;
            if !admissible(obs) {
                if level_complete(obs) && !transition.is_empty() && index + 1 == last {
                    continue;
                }
                self.deaths += 1;
                ended = true;
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
                && let std::collections::btree_map::Entry::Vacant(entry) = self.snapshots.entry(id)
            {
                let compressed = compress_snapshot(raw);
                self.snapshot_bytes += compressed.len();
                entry.insert(compressed);
                self.digests.insert(id, snapshot_digest(raw));
            }
            productive |= self.archive.live_entry_count() > retained_before;
            if let Some(id) = id {
                current_parent = id;
                suffix.clear();
            }
            self.won |= obs.won();
            self.points.push(Point {
                observation: obs,
                retained: id,
                frame,
            });
            if obs.won() {
                ended = true;
                break;
            }
        }
        if !ended && transition.is_empty() && steps.len() / stride != job.actions.len() {
            return Err(js_error("rollout result ended early"));
        }
        self.archive
            .record_selection_outcome(job.parent, productive);
        self.enforce_snapshot_budget();
        self.executions += 1;
        self.stopped = self.won;
        if self.stopped {
            self.pending.clear();
        }
        Ok(())
    }

    pub fn take_batch(&mut self) -> Result<String, JsValue> {
        serde_json::to_string(&Batch {
            executions: self.executions,
            states: self.snapshots.len(),
            deaths: self.deaths,
            frames: self.frames,
            snapshot_bytes: self.snapshot_bytes,
            stopped: self.stopped,
            won: self.won,
            points: std::mem::take(&mut self.points),
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
    fn human_histories_reject_invalid_inputs_and_bound_total_work() {
        assert!(
            validate_actions(&[Action {
                buttons: 8,
                frames: 200
            }])
            .is_ok()
        );
        for action in [
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
    fn drawn_actions_respect_the_controller_rules() {
        let mut rng = RomuDuoJrRand::with_seed(1);
        let mut prev = None;
        for _ in 0..10000 {
            let a = draw(&mut rng, prev);
            assert_eq!(a.buttons & 12, 0);
            assert_ne!(a.buttons & 0x30, 0x30);
            assert_ne!(a.buttons & 0xc0, 0xc0);
            assert!((2..=18).contains(&a.frames));
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
        fn complete(self, _: Option<(Self, &Self::Lineage)>) -> Self {
            self
        }
        fn record(_: &mut Self::Lineage, _: Self) {}
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn selector_matches_recorded_native_choices() {
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
                2, 1, 2, 2, 1, 2, 2, 1, 1, 2, 2, 1, 2, 2, 2, 1, 2, 1, 0, 2, 2, 0, 2, 0, 2, 2, 0, 2,
                2, 1, 1, 0
            ]
        );
    }
}
