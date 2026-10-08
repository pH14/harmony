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
    pub chips: u8,
    pub ability: u8,
    pub cleared: u8,
    pub available: u8,
    pub collectibles: u8,
    pub reload: bool,
}

impl ArchiveKey for Observation {
    type Place = (u8, u8, u8, u8, u8, u16, u16);
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
        chips: ram[0x508],
        ability: ram[0x800 + 0x1200],
        cleared: count(0x800 + 0x1f1f),
        available: count(0x800 + 0x1f27),
        collectibles: count(0x800 + 0x1f2f),
        reload: ram[0xa9] != 0,
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
    stopped: bool,
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
    snapshots: BTreeMap<usize, Vec<u8>>,
    rng: RomuDuoJrRand,
    executions: u32,
    deaths: u32,
    frames: u64,
    stopped: bool,
}

fn js_error(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

#[wasm_bindgen]
impl Explorer {
    #[wasm_bindgen(constructor)]
    pub fn new(seed: u32) -> Result<Explorer, JsValue> {
        let obs = decode(&memory()?).map_err(js_error)?;
        if obs.health == 0 || obs.x == 0 || obs.y == 0 || obs.selected_level != 0 {
            return Err(js_error("Nova setup did not reach level one"));
        }
        let mut archive = Archive::new(|a: &Action| u64::from(a.frames));
        archive.max_entries = 4096;
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
        Ok(Self {
            archive,
            rng: RomuDuoJrRand::with_seed(u64::from(seed)),
            executions: 0,
            deaths: 0,
            frames: 0,
            snapshots: BTreeMap::from([(0, capture()?)]),
            stopped: false,
        })
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
            restore(
                self.snapshots
                    .get(&parent)
                    .ok_or_else(|| js_error("selected snapshot missing"))?,
            )?;
            let prefix = self.archive.entry_input(parent).map_err(js_error)?.actions;
            let mut frame: u32 = prefix.iter().map(|a| u32::from(a.frames)).sum();
            let mut previous = prefix.last().copied();
            let mut suffix = Vec::new();
            let mut productive = false;
            let count = 1 + below(&mut self.rng, 8);
            for _ in 0..count {
                let action = draw(&mut self.rng, previous);
                step(action.buttons, action.frames)?;
                self.frames += u64::from(action.frames);
                frame += u32::from(action.frames);
                suffix.push(action);
                previous = Some(action);
                let obs = decode(&memory()?).map_err(js_error)?;
                if obs.health == 0
                    || obs.reload
                    || (obs.level != 0 && obs.level != 40)
                    || obs.selected_level != 0
                {
                    self.deaths += 1;
                    break;
                }
                let id = self
                    .archive
                    .insert(
                        Some(parent),
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
                    entry.insert(capture()?);
                }
                productive |= id.is_some();
                points.push(Point {
                    observation: obs,
                    retained: id,
                    frame,
                });
            }
            self.archive.record_selection_outcome(parent, productive);
            self.executions += 1;
            self.stopped = self.archive.live_entry_count() >= 4000 || self.executions >= 20000;
        }
        serde_json::to_string(&Batch {
            executions: self.executions,
            states: self.archive.active_count(),
            deaths: self.deaths,
            frames: self.frames,
            stopped: self.stopped,
            points,
        })
        .map_err(js_error)
    }

    pub fn state(&self, id: usize) -> Result<String, JsValue> {
        let observation = self
            .archive
            .entry_key(id)
            .ok_or_else(|| js_error("unknown state"))?;
        let actions = self.archive.entry_input(id).map_err(js_error)?.actions;
        let frames = actions.iter().map(|a| u32::from(a.frames)).sum();
        serde_json::to_string(&State {
            id,
            observation,
            actions,
            frames,
        })
        .map_err(js_error)
    }

    pub fn snapshot(&self, id: usize) -> Result<Vec<u8>, JsValue> {
        self.snapshots
            .get(&id)
            .cloned()
            .ok_or_else(|| js_error("snapshot unavailable"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let state = decode(&ram).unwrap();
        assert_eq!(
            (state.x, state.y, state.health, state.ability, state.cleared),
            (56, 148, 4, 2, 2)
        );
        assert!(decode(&ram[..0x800]).is_err());
    }
    #[test]
    fn heatmap_position_is_not_full_archive_identity() {
        let a = decode(&vec![0; 0x2800]).unwrap();
        let b = Observation { health: 4, ..a };
        assert_eq!(a.place(), b.place());
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
}
