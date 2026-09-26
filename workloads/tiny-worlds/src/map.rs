// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::Key;
use searcher::search::rand::RomuDuoJrRand;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::rc::Rc;

const SUBPLACES: u16 = 16;
const GAUNTLET_HIT: u8 = 1;
const GAUNTLET_ODDS: u64 = 64;
const GAUNTLET_HITS: u64 = 63;
const OUTER_ODDS: u64 = 4;
const OUTER_HIT: u8 = 1;
const FARM_ODDS: u64 = 8;
const TAIL_ACTIONS: u8 = 3;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub width: u8,
    pub height: u8,
    pub layout: u64,
    pub loops: u8,
    pub corridor: u8,
    pub shaft: u8,
    pub inner: u8,
    #[serde(default)]
    pub farms: u8,
    #[serde(default)]
    pub farm_cap: u8,
    #[serde(default = "one")]
    pub items: u8,
    #[serde(default)]
    pub boss_stock: u8,
    #[serde(default)]
    pub item_optional: bool,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub boss_hits_back: bool,
    #[serde(default)]
    pub timing: u8,
    #[serde(default)]
    pub gauntlet: bool,
}

fn one() -> u8 {
    1
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub cell: u8,
    pub arm: u8,
    pub progress: u8,
    pub item: bool,
    pub goal: bool,
    pub health: u8,
    pub stock: u8,
    pub found: u16,
    pub hits: u8,
    pub phase: u8,
    pub tail: u8,
    pub dead: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Layout {
    pub doors: Vec<u8>,
    pub inner: Vec<bool>,
    pub door_cell: u8,
    pub door_direction: u8,
    pub item: Option<u8>,
    pub goal: u8,
    pub start_to_door: u8,
    pub door_to_item: Option<u8>,
    pub door_to_goal: u8,
    pub loops: u8,
    pub entry: u8,
    pub farms: Vec<u8>,
    pub items: Vec<u8>,
    pub key: Option<u8>,
}

thread_local! {
    static LAYOUT: RefCell<Option<(Config, Rc<Layout>)>> = const { RefCell::new(None) };
}

fn next_item(layout: &Layout, found: u16) -> Option<u8> {
    layout
        .items
        .get(usize::try_from(found.count_ones()).expect("item count fits"))
        .copied()
}

fn pick(rand: &mut RomuDuoJrRand, len: usize) -> usize {
    rand.below(NonZeroUsize::new(len).expect("non-empty choice"))
}

fn opposite(direction: u8) -> u8 {
    (direction + 2) % 4
}

impl Config {
    fn cells(&self) -> u8 {
        self.width * self.height
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(2..=8).contains(&self.width) || !(2..=8).contains(&self.height) {
            return Err("map width and height must be 2..=8".into());
        }
        if !(1..=4).contains(&self.corridor) || !(1..=4).contains(&self.shaft) {
            return Err("map corridor and shaft lengths must be 1..=4".into());
        }
        if self.loops > 16 {
            return Err("map loops must be at most 16".into());
        }
        if !(2..=self.cells() - 2).contains(&self.inner) {
            return Err("map inner region must hold 2 to two fewer than all rooms".into());
        }
        if !(1..=9).contains(&self.items) {
            return Err("map items must be 1..=9".into());
        }
        if self.items > 1
            && ((self.farms > 0 && !self.gauntlet) || self.items + 2 > self.cells() - self.inner)
        {
            return Err("several map items need no farms and enough outer rooms".into());
        }
        if self.timing == 1 || self.timing > 16 {
            return Err("map timing must be 0 or 2..=16".into());
        }
        if self.item_optional && (self.items > 1 || self.boss_stock > 0) {
            return Err("an optional map item needs one item and no boss".into());
        }
        if self.locked && (self.items > 1 || self.item_optional || self.boss_stock > 0) {
            return Err("a locked map region needs one required item and no boss".into());
        }
        if self.gauntlet && (self.items < 2 || self.farms == 0) {
            return Err("a map gauntlet needs several items and farms".into());
        }
        if self.boss_hits_back && !(1..=6).contains(&self.boss_stock) {
            return Err("a map boss that hits back needs a boss stock of 1..=6".into());
        }
        if self.boss_stock > 0
            && (self.items > 1 || self.farms < 2 || self.boss_stock > self.farm_cap)
        {
            return Err("a map boss needs one item and farms whose cap covers its stock".into());
        }
        if self.farms > 8 || (self.farms > 0 && !(1..=63).contains(&self.farm_cap)) {
            return Err("map farms must be at most 8 with a cap of 1..=63".into());
        }
        let layout = self.layout();
        if self.items > 1 && layout.items.len() < usize::from(self.items) {
            return Err("the map's outer region has too few rooms for its items".into());
        }
        if layout.farms.len() < usize::from(self.farms) {
            return Err("the map has too few rooms for its farms".into());
        }
        if self.farms == 0 && self.farm_cap != 0 {
            return Err("a map farm cap needs farms".into());
        }
        if self.locked && layout.key.is_none() {
            return Err("a locked map needs an outer room for the key".into());
        }
        Ok(())
    }

    fn neighbour(&self, cell: u8, direction: u8) -> Option<u8> {
        let (x, y) = (cell % self.width, cell / self.width);
        match direction {
            0 if y > 0 => Some(cell - self.width),
            1 if x + 1 < self.width => Some(cell + 1),
            2 if y + 1 < self.height => Some(cell + self.width),
            3 if x > 0 => Some(cell - 1),
            _ => None,
        }
    }

    fn length(&self, direction: u8) -> u8 {
        if direction.is_multiple_of(2) {
            self.shaft
        } else {
            self.corridor
        }
    }

    fn distances(&self, doors: &[u8], from: u8) -> Vec<u8> {
        let mut distance = vec![u8::MAX; doors.len()];
        distance[usize::from(from)] = 0;
        let mut pending = VecDeque::from([from]);
        while let Some(cell) = pending.pop_front() {
            for direction in 0..4 {
                if doors[usize::from(cell)] & (1 << direction) == 0 {
                    continue;
                }
                let next = self.neighbour(cell, direction).expect("door has a room");
                if distance[usize::from(next)] == u8::MAX {
                    distance[usize::from(next)] = distance[usize::from(cell)] + 1;
                    pending.push_back(next);
                }
            }
        }
        distance
    }

    pub fn layout(&self) -> Rc<Layout> {
        LAYOUT.with_borrow_mut(|cached| match cached {
            Some((config, layout)) if config == self => Rc::clone(layout),
            _ => {
                let layout = Rc::new(self.build());
                *cached = Some((*self, Rc::clone(&layout)));
                layout
            }
        })
    }

    fn build(&self) -> Layout {
        let cells = usize::from(self.cells());
        let mut rand = RomuDuoJrRand::with_seed(self.layout);
        let mut doors = vec![0_u8; cells];
        let mut parent = vec![None::<(u8, u8)>; cells];
        let mut visited = vec![false; cells];
        let mut order = Vec::with_capacity(cells);
        let mut stack = vec![0_u8];
        visited[0] = true;
        order.push(0);
        while let Some(&cell) = stack.last() {
            let open: Vec<u8> = (0..4)
                .filter(|&d| {
                    self.neighbour(cell, d)
                        .is_some_and(|n| !visited[usize::from(n)])
                })
                .collect();
            if open.is_empty() {
                stack.pop();
                continue;
            }
            let direction = open[pick(&mut rand, open.len())];
            let child = self.neighbour(cell, direction).expect("open neighbour");
            doors[usize::from(cell)] |= 1 << direction;
            doors[usize::from(child)] |= 1 << opposite(direction);
            parent[usize::from(child)] = Some((cell, direction));
            visited[usize::from(child)] = true;
            order.push(child);
            stack.push(child);
        }
        let mut subtree = vec![1_u8; cells];
        for &cell in order.iter().rev() {
            if let Some((up, _)) = parent[usize::from(cell)] {
                subtree[usize::from(up)] += subtree[usize::from(cell)];
            }
        }
        let tree_depth = self.distances(&doors, 0);
        let entry = (1..self.cells())
            .filter(|&cell| subtree[usize::from(cell)] <= self.cells() - 2)
            .min_by_key(|&cell| {
                let miss = subtree[usize::from(cell)].abs_diff(self.inner);
                if (self.item_optional || self.locked) && miss <= 2 {
                    (0, tree_depth[usize::from(cell)], cell)
                } else {
                    (1 + miss, 0, cell)
                }
            })
            .expect("a subtree leaves two outer rooms");
        let (door_cell, door_direction) = parent[usize::from(entry)].expect("entry has a parent");
        let mut branch = doors.clone();
        branch[usize::from(entry)] &= !(1 << opposite(door_direction));
        let inner: Vec<bool> = self
            .distances(&branch, entry)
            .into_iter()
            .map(|d| d != u8::MAX)
            .collect();
        let mut candidates: Vec<(u8, u8)> = (0..self.cells())
            .flat_map(|cell| [(cell, 1), (cell, 2)])
            .filter(|&(cell, direction)| {
                self.neighbour(cell, direction).is_some_and(|other| {
                    doors[usize::from(cell)] & (1 << direction) == 0
                        && inner[usize::from(cell)] == inner[usize::from(other)]
                })
            })
            .collect();
        let mut added = 0;
        while added < self.loops && !candidates.is_empty() {
            let (cell, direction) = candidates.swap_remove(pick(&mut rand, candidates.len()));
            let other = self
                .neighbour(cell, direction)
                .expect("candidate has a room");
            doors[usize::from(cell)] |= 1 << direction;
            doors[usize::from(other)] |= 1 << opposite(direction);
            added += 1;
        }
        let from_entry = self.distances(&doors, entry);
        let from_door = self.distances(&doors, door_cell);
        let farthest = |from: &[u8], inside: bool| {
            (0..self.cells())
                .filter(|&c| inner[usize::from(c)] == inside)
                .filter(|&c| inside || self.boss_stock == 0 || doors[usize::from(c)] != 0b1111)
                .max_by_key(|&c| (from[usize::from(c)], std::cmp::Reverse(c)))
                .expect("rooms on each side")
        };
        let key = if self.locked {
            self.key_room(&doors, &inner, door_cell)
        } else {
            None
        };
        let (item, goal, items) = if self.items == 1 {
            let goal = if self.item_optional {
                let from_start = self.distances(&doors, 0);
                (0..self.cells())
                    .filter(|&c| !inner[usize::from(c)])
                    .filter(|&c| {
                        u16::from(from_start[usize::from(c)]) + u16::from(from_door[usize::from(c)])
                            > u16::from(from_start[usize::from(door_cell)])
                    })
                    .max_by_key(|&c| (from_start[usize::from(c)], std::cmp::Reverse(c)))
                    .unwrap_or_else(|| farthest(&from_start, false))
            } else if self.locked {
                (0..self.cells())
                    .filter(|&c| !inner[usize::from(c)] && Some(c) != key)
                    .max_by_key(|&c| (from_door[usize::from(c)], std::cmp::Reverse(c)))
                    .expect("an outer room besides the key")
            } else {
                farthest(&from_door, false)
            };
            (Some(farthest(&from_entry, true)), goal, Vec::new())
        } else {
            let mut outer = doors.clone();
            outer[usize::from(door_cell)] &= !(1 << door_direction);
            let mut chosen = vec![0_u8];
            let mut nearest = self.distances(&outer, 0);
            for _ in 0..self.items {
                let Some(next) = (0..self.cells())
                    .filter(|&c| !inner[usize::from(c)] && !chosen.contains(&c))
                    .max_by_key(|&c| (nearest[usize::from(c)], std::cmp::Reverse(c)))
                else {
                    break;
                };
                chosen.push(next);
                for (n, d) in nearest.iter_mut().zip(self.distances(&outer, next)) {
                    *n = (*n).min(d);
                }
            }
            let mut items = chosen.split_off(1);
            if self.gauntlet
                && let Some(mouth) = (1..self.cells())
                    .filter(|&c| !inner[usize::from(c)] && Some(&c) != items.first())
                    .min_by_key(|&c| (from_door[usize::from(c)], c))
            {
                items.truncate(usize::from(self.items) - 1);
                items.push(mouth);
            }
            (None, farthest(&from_entry, true), items)
        };
        let mut rooms: Vec<u8> = if self.boss_stock > 0 {
            let from_goal = self.distances(&doors, goal);
            let mut outer: Vec<u8> = (0..self.cells())
                .filter(|&c| !inner[usize::from(c)] && c != goal)
                .collect();
            outer.sort_by_key(|&c| (std::cmp::Reverse(from_goal[usize::from(c)]), c));
            outer.truncate((outer.len() / 2).max(usize::from(self.farms)));
            outer
        } else if self.gauntlet {
            let mut outer: Vec<u8> = (0..self.cells())
                .filter(|&c| {
                    !inner[usize::from(c)] && c != 0 && c != door_cell && !items.contains(&c)
                })
                .collect();
            outer.sort_by_key(|&c| (std::cmp::Reverse(from_door[usize::from(c)]), c));
            outer.truncate((outer.len() / 2).max(usize::from(self.farms)));
            outer
        } else {
            (0..self.cells())
                .filter(|&c| inner[usize::from(c)] && Some(c) != item)
                .collect()
        };
        let mut farms = Vec::new();
        while farms.len() < usize::from(self.farms) && !rooms.is_empty() {
            farms.push(rooms.swap_remove(pick(&mut rand, rooms.len())));
        }
        Layout {
            start_to_door: from_door[0],
            door_to_item: item.map(|item| from_entry[usize::from(item)] + 1),
            door_to_goal: if self.items == 1 {
                from_door[usize::from(goal)]
            } else {
                from_entry[usize::from(goal)] + 1
            },
            doors,
            inner,
            door_cell,
            door_direction,
            item,
            goal,
            loops: added,
            entry,
            farms,
            items,
            key,
        }
    }

    fn key_room(&self, doors: &[u8], inner: &[bool], door_cell: u8) -> Option<u8> {
        let from_start = self.distances(doors, 0);
        (1..self.cells())
            .filter(|&c| !inner[usize::from(c)] && c != door_cell)
            .max_by_key(|&c| (from_start[usize::from(c)], std::cmp::Reverse(c)))
    }

    pub fn top_tier(&self) -> u16 {
        if self.items == 1 {
            1 + u16::from(self.locked)
        } else {
            u16::from(self.items)
        }
    }

    pub fn initial(&self) -> State {
        State {
            cell: 0,
            arm: 0,
            progress: 0,
            item: false,
            goal: false,
            health: 0,
            stock: 0,
            found: 0,
            hits: 0,
            phase: 0,
            tail: 0,
            dead: false,
        }
    }

    fn cap(&self) -> u8 {
        if self.farms > 0 { self.farm_cap } else { 0 }
    }

    fn complete(&self, s: State) -> bool {
        if self.items == 1 {
            (s.item || self.item_optional) && s.hits == self.boss_stock
        } else {
            u32::from(s.found) + 1 == 1 << self.items
        }
    }

    fn open(&self, layout: &Layout, s: State, cell: u8, direction: u8) -> bool {
        let gated = (cell == layout.door_cell && direction == layout.door_direction)
            || (cell == layout.entry && direction == opposite(layout.door_direction));
        layout.doors[usize::from(cell)] & (1 << direction) != 0
            && !(self.items > 1 && gated && !self.complete(s))
            && !(self.locked && gated && s.found == 0)
    }

    pub fn tier(&self, s: State) -> u16 {
        if self.items == 1 {
            u16::from(s.item) + u16::from(self.locked && s.found == 1)
        } else {
            u16::try_from(s.found.count_ones()).expect("item count fits")
        }
    }

    fn health_cap(&self, layout: &Layout) -> u8 {
        if self.boss_hits_back {
            self.boss_stock + 2
        } else if self.gauntlet {
            layout.door_to_goal + 2
        } else {
            self.cap()
        }
    }

    fn state_fits(&self, layout: &Layout, s: State) -> bool {
        if s.cell >= self.cells() || s.arm > 4 || s.phase >= self.timing.max(1) {
            return false;
        }
        let arm_fits = if s.arm == 0 {
            s.progress == 0
        } else {
            let direction = s.arm - 1;
            self.open(layout, s, s.cell, direction)
                && (1..self.length(direction)).contains(&s.progress)
        };
        let items_fit = if self.items == 1 {
            s.found == 0 || (self.locked && s.found == 1)
        } else {
            !s.item
                && u32::from(s.found) < 1 << self.items
                && s.found & (s.found + 1) == 0
                && next_item(layout, s.found) != Some(s.cell)
                && (self.complete(s) || !layout.inner[usize::from(s.cell)])
        };
        arm_fits
            && items_fit
            && s.hits <= self.boss_stock
            && (s.hits == 0 || (s.item && s.arm == 0 && s.cell == layout.goal))
            && s.health <= self.health_cap(layout)
            && s.hits + s.stock <= self.cap()
            && (!self.boss_hits_back || s.hits + s.health <= self.health_cap(layout))
            && s.tail < 1 << (2 * TAIL_ACTIONS)
            && (self.gauntlet || (s.tail == 0 && !s.dead))
            && (!s.dead || s.health == 0)
            && (self.boss_stock == 0 || s.item || s.stock == 0)
            && (s.item || Some(s.cell) != layout.item)
            && (!self.locked
                || s.found == 1
                || (Some(s.cell) != layout.key && !s.item && !layout.inner[usize::from(s.cell)]))
            && s.goal == (self.complete(s) && s.arm == 0 && s.cell == layout.goal)
    }

    pub fn state_is_bounded(&self, s: State) -> bool {
        self.validate().is_ok() && self.state_fits(&self.layout(), s)
    }

    pub fn goal(&self, s: State) -> bool {
        self.state_is_bounded(s) && s.goal
    }

    fn arrive(&self, layout: &Layout, s: State, cell: u8) -> State {
        let mut next = State {
            cell,
            arm: 0,
            progress: 0,
            item: s.item || Some(cell) == layout.item,
            ..s
        };
        if Some(cell) == layout.key {
            next.found = 1;
        }
        if next_item(layout, next.found) == Some(cell) {
            next.found |= 1 << next.found.count_ones();
        }
        if let Some(farm) = layout.farms.iter().position(|&room| room == cell) {
            if self.gauntlet {
                if self.roll(cell, s.tail ^ 0x80).is_multiple_of(FARM_ODDS) {
                    next.health = (next.health + 1).min(self.health_cap(layout));
                }
            } else if farm % 2 == 0 {
                next.health = (next.health + 1).min(self.health_cap(layout));
            } else if self.boss_stock == 0 || next.item {
                next.stock = (next.stock + 1).min(self.cap());
                next.health = next.health.saturating_sub(u8::from(self.boss_hits_back));
            }
        }
        next.goal = self.complete(next) && cell == layout.goal;
        next
    }

    fn hit(&self, layout: &Layout, cell: u8, tail: u8) -> bool {
        self.gauntlet
            && layout.inner[usize::from(cell)]
            && self.roll(cell, tail) % GAUNTLET_ODDS < GAUNTLET_HITS
    }

    fn roll(&self, cell: u8, tail: u8) -> u64 {
        let mut x = self.layout ^ (u64::from(cell) << 8) ^ u64::from(tail);
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
        x ^= x >> 33;
        x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        x ^ (x >> 33)
    }

    fn enter(&self, layout: &Layout, s: State, cell: u8, hits: bool) -> State {
        if hits
            && self.gauntlet
            && !layout.inner[usize::from(cell)]
            && self.roll(cell, s.tail).is_multiple_of(OUTER_ODDS)
        {
            return self.arrive(
                layout,
                State {
                    health: s.health.saturating_sub(OUTER_HIT),
                    ..s
                },
                cell,
            );
        }
        if !hits || !self.hit(layout, cell, s.tail) {
            return self.arrive(layout, s, cell);
        }
        if s.health <= GAUNTLET_HIT {
            return State {
                health: 0,
                dead: true,
                ..s
            };
        }
        self.arrive(
            layout,
            State {
                health: s.health - GAUNTLET_HIT,
                ..s
            },
            cell,
        )
    }

    pub fn step(&self, s: State, action: u8) -> State {
        self.step_with(s, action, true)
    }

    fn step_with(&self, s: State, action: u8, hits: bool) -> State {
        let layout = self.layout();
        if action > 3 || !self.state_fits(&layout, s) || s.goal || s.dead {
            return s;
        }
        let action = (action + s.phase) % 4;
        let tail = if self.gauntlet {
            ((s.tail << 2) | action) & ((1 << (2 * TAIL_ACTIONS)) - 1)
        } else {
            0
        };
        let next = self.advance(&layout, State { tail, ..s }, action, hits);
        State {
            phase: if self.timing == 0 {
                0
            } else {
                (s.phase + 1) % self.timing
            },
            ..next
        }
    }

    fn advance(&self, layout: &Layout, s: State, action: u8, hits: bool) -> State {
        if s.arm == 0 {
            if !self.open(layout, s, s.cell, action) {
                if self.boss_stock > 0
                    && s.item
                    && s.cell == layout.goal
                    && s.stock > 0
                    && (!self.boss_hits_back || s.health > 0)
                {
                    let fired = State {
                        stock: s.stock - 1,
                        health: s.health - u8::from(self.boss_hits_back),
                        hits: s.hits + 1,
                        ..s
                    };
                    return State {
                        goal: self.complete(fired),
                        ..fired
                    };
                }
                return s;
            }
            if self.length(action) == 1 {
                let next = self.neighbour(s.cell, action).expect("door has a room");
                return self.enter(layout, State { hits: 0, ..s }, next, hits);
            }
            return State {
                arm: action + 1,
                progress: 1,
                hits: 0,
                ..s
            };
        }
        let direction = s.arm - 1;
        if action == direction {
            if s.progress + 1 == self.length(direction) {
                let next = self.neighbour(s.cell, direction).expect("door has a room");
                return self.enter(layout, s, next, hits);
            }
            return State {
                progress: s.progress + 1,
                ..s
            };
        }
        if action == opposite(direction) {
            return if s.progress == 1 {
                State {
                    arm: 0,
                    progress: 0,
                    ..s
                }
            } else {
                State {
                    progress: s.progress - 1,
                    ..s
                }
            };
        }
        s
    }

    pub fn place(s: State) -> u16 {
        if s.dead {
            return 64 * SUBPLACES + 32;
        }
        if s.hits > 0 {
            return 64 * SUBPLACES + u16::from(s.hits);
        }
        let sub = if s.arm == 0 {
            0
        } else {
            1 + u16::from(s.arm - 1) * 3 + u16::from(s.progress - 1)
        };
        u16::from(s.cell) * SUBPLACES + sub
    }

    pub fn key(&self, s: State, broken: bool) -> Key {
        Key {
            stock: s.stock,
            place: Self::place(s),
            context: 0,
            charge: 0,
            health: s.health,
            goal: s.goal,
            tier: if broken { 0 } else { self.tier(s) },
        }
    }

    pub fn reachable(&self) -> Result<bool, String> {
        self.validate()?;
        crate::reachable(
            self.initial(),
            |s| self.goal(s),
            |s, a| {
                let next = self.step_with(s, a, false);
                State {
                    health: if self.boss_hits_back { next.health } else { 0 },
                    tail: 0,
                    stock: next.stock.min(self.boss_stock),
                    phase: 0,
                    ..next
                }
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(layout: u64) -> Config {
        Config {
            width: 5,
            height: 4,
            layout,
            loops: 2,
            corridor: 2,
            shaft: 3,
            inner: 8,
            farms: 0,
            farm_cap: 0,
            items: 1,
            boss_stock: 0,
            item_optional: false,
            locked: false,
            boss_hits_back: false,
            timing: 0,
            gauntlet: false,
        }
    }

    fn walk(w: &Config, layout: &Layout, from: State, to: u8) -> State {
        let mut previous = vec![None; layout.doors.len()];
        let mut pending = VecDeque::from([from.cell]);
        previous[usize::from(from.cell)] = Some(from.cell);
        while let Some(cell) = pending.pop_front() {
            for d in 0..4 {
                if layout.doors[usize::from(cell)] & (1 << d) == 0 {
                    continue;
                }
                let n = w.neighbour(cell, d).unwrap();
                if previous[usize::from(n)].is_none() {
                    previous[usize::from(n)] = Some(cell);
                    pending.push_back(n);
                }
            }
        }
        let mut path = vec![to];
        while *path.last().unwrap() != from.cell {
            path.push(previous[usize::from(*path.last().unwrap())].unwrap());
        }
        path.reverse();
        let mut state = from;
        for pair in path.windows(2) {
            let d = (0..4)
                .find(|&d| w.neighbour(pair[0], d) == Some(pair[1]))
                .unwrap();
            for _ in 0..w.length(d) {
                state = w.step(state, d);
            }
            assert_eq!(state.cell, pair[1]);
        }
        state
    }

    #[test]
    fn layouts_hang_the_inner_region_from_one_door() {
        for layout in [1, 2, 3, 0xdead_beef] {
            let w = config(layout);
            let l = w.layout();
            let doors: u32 = l.doors.iter().map(|d| d.count_ones()).sum();
            assert_eq!(doors / 2, u32::from(w.cells()) - 1 + u32::from(l.loops));
            assert!(l.loops <= w.loops);
            assert!(
                !l.inner[0]
                    && l.inner[usize::from(l.item.unwrap())]
                    && !l.inner[usize::from(l.goal)]
            );
            assert!(!l.inner[usize::from(l.door_cell)]);
            let entry = w.neighbour(l.door_cell, l.door_direction).unwrap();
            assert!(l.inner[usize::from(entry)]);
            let mut crossings = 0;
            for cell in 0..w.cells() {
                for d in 0..4 {
                    if l.doors[usize::from(cell)] & (1 << d) != 0 {
                        let n = w.neighbour(cell, d).unwrap();
                        crossings +=
                            u32::from(l.inner[usize::from(cell)] != l.inner[usize::from(n)]);
                    }
                }
            }
            assert_eq!(crossings, 2);
            assert!(l.door_to_item.unwrap() > 0 && l.door_to_goal > 0);
            assert_eq!(w.build(), *l);
            assert!(w.reachable().unwrap());
        }
    }

    #[test]
    fn the_goal_counts_only_with_the_item() {
        let w = config(7);
        let l = w.layout();
        let early = walk(&w, &l, w.initial(), l.goal);
        assert!(!early.item && !early.goal && !w.goal(early));
        let with_item = walk(&w, &l, early, l.item.unwrap());
        assert!(with_item.item);
        assert_eq!(w.key(with_item, false).tier, 1);
        assert_eq!(w.key(with_item, true).tier, 0);
        assert_eq!(
            w.key(with_item, true),
            w.key(
                State {
                    item: false,
                    ..with_item
                },
                false
            )
        );
        let done = walk(&w, &l, with_item, l.goal);
        assert!(w.goal(done));
    }

    #[test]
    fn arms_advance_retreat_and_ignore_side_presses() {
        let w = config(11);
        let l = w.layout();
        let direction = (0..4)
            .find(|&d| l.doors[0] & (1 << d) != 0 && w.length(d) > 1)
            .unwrap();
        let s = w.step(w.initial(), direction);
        assert_eq!((s.arm, s.progress), (direction + 1, 1));
        assert_eq!(w.step(s, (direction + 1) % 4), s);
        assert_eq!(w.step(s, opposite(direction)), w.initial());
        assert_ne!(Config::place(s), Config::place(w.initial()));
        for cell in (0..w.cells()).filter(|&c| Some(c) != l.item) {
            if let Some(d) = (0..4).find(|&d| l.doors[usize::from(cell)] & (1 << d) == 0) {
                let at = State {
                    cell,
                    ..w.initial()
                };
                assert_eq!(w.step(at, d), at);
            }
        }
    }

    #[test]
    fn real_campaign_replays_and_orders_its_milestones() {
        for broken in [false, true] {
            let workload = crate::Workload {
                config: crate::worlds::World::Map(config(crate::test_seed())),
                broken,
                scale: None,
            };
            let report = crate::run(&workload, crate::test_seed(), 3000, true).unwrap();
            assert_eq!(report["verified"], true);
            let first: Vec<Option<u64>> =
                serde_json::from_value(report["evidence"]["map_first"].clone()).unwrap();
            let reached: Vec<u64> = first.iter().map_while(|w| *w).collect();
            assert!(reached.windows(2).all(|w| w[0] <= w[1]));
            assert!(first.iter().skip(reached.len()).all(Option::is_none));
            assert_eq!(first[3], report["first_objective_work"].as_u64());
            let tiers: Vec<Option<u64>> =
                serde_json::from_value(report["evidence"]["map_first_tier"].clone()).unwrap();
            assert_eq!(tiers[0], Some(0));
            assert_eq!(tiers[1], first[1]);
            let timeline: Vec<[u64; 3]> =
                serde_json::from_value(report["parent_timeline"].clone()).unwrap();
            assert!(timeline.windows(2).all(|w| w[0][0] <= w[1][0]));
            assert!(timeline.iter().all(|t| t[1] <= 1));
        }
    }

    #[test]
    fn several_items_open_the_goal_region() {
        for layout in [1, 2, 3, 0xdead_beef] {
            let w = Config {
                width: 8,
                height: 8,
                inner: 4,
                items: 6,
                ..config(layout)
            };
            let l = w.layout();
            assert_eq!(l.items.len(), 6);
            assert!(l.items.iter().all(|&c| !l.inner[usize::from(c)] && c != 0));
            assert!(l.inner[usize::from(l.goal)]);
            assert!(w.reachable().unwrap());
            let door = walk(&w, &l, w.initial(), l.door_cell);
            assert_eq!(w.step(door, l.door_direction).cell, door.cell);
            assert_eq!(w.step(door, l.door_direction).arm, 0);
            let mut s = door;
            for &room in &l.items {
                s = walk(&w, &l, s, room);
            }
            assert_eq!(w.key(s, false).tier, 6);
            s = walk(&w, &l, s, l.door_cell);
            let mut through = s;
            for _ in 0..w.length(l.door_direction) {
                through = w.step(through, l.door_direction);
            }
            assert_eq!(through.cell, l.entry);
        }
    }

    #[test]
    fn farms_raise_health_and_stock_to_the_cap() {
        let w = Config {
            farms: 2,
            farm_cap: 3,
            ..config(9)
        };
        let l = w.layout();
        assert_eq!(l.farms.len(), 2);
        assert!(w.reachable().unwrap());
        let mut s = w.initial();
        for _ in 0..5 {
            s = walk(&w, &l, s, l.farms[0]);
            s = walk(&w, &l, s, l.farms[1]);
        }
        assert_eq!((s.health, s.stock), (3, 3));
        assert!(w.state_is_bounded(s));
        assert_eq!(w.key(s, false).health, 3);
    }

    #[test]
    fn a_gauntlet_charges_hits_by_the_arrival_tail_and_kills_below_the_hit() {
        let w = Config {
            items: 2,
            farms: 2,
            farm_cap: 1,
            gauntlet: true,
            ..config(9)
        };
        let l = w.layout();
        assert!(w.reachable().unwrap());
        assert!(
            l.farms
                .iter()
                .all(|&c| !l.inner[usize::from(c)] && !l.items.contains(&c))
        );
        let from_door = w.distances(&l.doors, l.door_cell);
        let mouth = l.items[1];
        assert!(!l.inner[usize::from(mouth)]);
        assert!(
            (1..w.cells())
                .filter(|&c| !l.inner[usize::from(c)] && c != l.items[0])
                .all(|c| from_door[usize::from(c)] >= from_door[usize::from(mouth)])
        );
        let tails: Vec<u8> = (0..64).collect();
        let farm = l.farms[0];
        let refill = *tails
            .iter()
            .find(|&&t| w.roll(farm, t ^ 0x80).is_multiple_of(FARM_ODDS))
            .unwrap();
        let dry = *tails
            .iter()
            .find(|&&t| !w.roll(farm, t ^ 0x80).is_multiple_of(FARM_ODDS))
            .unwrap();
        let start = w.initial();
        assert_eq!(
            w.enter(
                &l,
                State {
                    tail: refill,
                    ..start
                },
                farm,
                true
            )
            .health,
            1
        );
        assert_eq!(
            w.enter(&l, State { tail: dry, ..start }, farm, true).health,
            0
        );
        let mut s = start;
        for &room in &l.items {
            s = walk(&w, &l, s, room);
        }
        let door = walk(&w, &l, s, l.door_cell);
        let hit = *tails.iter().find(|&&t| w.hit(&l, l.entry, t)).unwrap();
        let miss = *tails.iter().find(|&&t| !w.hit(&l, l.entry, t)).unwrap();
        let entered = |health, tail| {
            w.enter(
                &l,
                State {
                    health,
                    tail,
                    ..door
                },
                l.entry,
                true,
            )
        };
        assert_eq!(entered(0, miss).cell, l.entry);
        assert_eq!(entered(0, miss).health, 0);
        assert_eq!(entered(GAUNTLET_HIT + 1, hit).health, 1);
        assert_eq!(entered(GAUNTLET_HIT + 1, hit).cell, l.entry);
        let dead = entered(GAUNTLET_HIT, hit);
        assert!(dead.dead && dead.health == 0 && dead.cell == l.door_cell);
        assert!(w.state_is_bounded(dead));
        assert_eq!(w.step(dead, l.door_direction), dead);
        assert_eq!(Config::place(dead), 64 * SUBPLACES + 32);
        assert!(!w.hit(&l, l.door_cell, hit));
    }

    #[test]
    fn states_outside_the_layout_are_rejected() {
        let w = config(5);
        let l = w.layout();
        assert!(!w.state_is_bounded(State {
            cell: l.item.unwrap(),
            ..w.initial()
        }));
        let direction = (0..4)
            .find(|&d| l.doors[usize::from(l.item.unwrap())] & (1 << d) != 0 && w.length(d) > 1)
            .unwrap();
        assert!(!w.state_is_bounded(State {
            cell: l.item.unwrap(),
            arm: direction + 1,
            progress: 1,
            ..w.initial()
        }));
        assert!(!w.state_is_bounded(State {
            cell: l.goal,
            item: true,
            ..w.initial()
        }));
        assert!(!w.state_is_bounded(State {
            progress: 1,
            ..w.initial()
        }));
        assert!(!w.state_is_bounded(State {
            cell: w.cells(),
            ..w.initial()
        }));
    }

    #[test]
    fn each_state_bound_rejects_its_own_violation() {
        let boss = Config {
            farms: 2,
            farm_cap: 3,
            boss_stock: 3,
            boss_hits_back: true,
            ..config(crate::test_seed())
        };
        let l = boss.layout();
        let fighting = State {
            cell: l.goal,
            item: true,
            stock: 1,
            health: 1,
            hits: 2,
            ..boss.initial()
        };
        assert!(boss.state_is_bounded(fighting));
        for s in [
            State {
                hits: 4,
                ..fighting
            },
            State {
                cell: l.farms[0],
                ..fighting
            },
            State {
                stock: 2,
                ..fighting
            },
            State {
                health: 4,
                ..fighting
            },
            State {
                hits: 0,
                stock: 4,
                ..fighting
            },
            State {
                hits: 0,
                health: 6,
                ..fighting
            },
            State {
                cell: l.farms[1],
                item: false,
                hits: 0,
                ..fighting
            },
            State {
                found: 1,
                ..fighting
            },
        ] {
            assert!(!boss.state_is_bounded(s), "{s:?}");
        }
        let ordered = Config {
            width: 8,
            height: 8,
            inner: 4,
            items: 3,
            ..config(crate::test_seed())
        };
        let l = ordered.layout();
        let one = State {
            cell: l.items[0],
            found: 1,
            ..ordered.initial()
        };
        assert!(ordered.state_is_bounded(one));
        for s in [
            State { found: 2, ..one },
            State { item: true, ..one },
            State {
                cell: l.items[1],
                ..one
            },
            State {
                cell: l.entry,
                ..one
            },
        ] {
            assert!(!ordered.state_is_bounded(s), "{s:?}");
        }
        let locked = Config {
            width: 8,
            height: 8,
            inner: 4,
            locked: true,
            ..config(crate::test_seed())
        };
        let l = locked.layout();
        for s in [
            State {
                cell: l.key.unwrap(),
                ..locked.initial()
            },
            State {
                cell: l.entry,
                ..locked.initial()
            },
            State {
                found: 2,
                ..locked.initial()
            },
        ] {
            assert!(!locked.state_is_bounded(s), "{s:?}");
        }
    }

    fn fire(w: &Config, l: &Layout, s: State) -> State {
        let wall = (0..4)
            .find(|&d| l.doors[usize::from(l.goal)] & (1 << d) == 0)
            .unwrap();
        w.step(s, wall)
    }

    #[test]
    fn a_boss_needs_stock_farmed_with_the_item_and_spent_in_its_room() {
        let w = Config {
            farms: 2,
            farm_cap: 3,
            boss_stock: 3,
            ..config(crate::test_seed())
        };
        let l = w.layout();
        assert!(w.reachable().unwrap());
        assert!(
            l.farms
                .iter()
                .all(|&c| !l.inner[usize::from(c)] && c != l.goal)
        );
        let mut s = walk(&w, &l, w.initial(), l.farms[1]);
        assert_eq!(s.stock, 0);
        s = walk(&w, &l, s, l.item.unwrap());
        while s.stock < 2 {
            s = walk(&w, &l, s, l.farms[1]);
            s = walk(&w, &l, s, l.farms[0]);
        }
        s = walk(&w, &l, s, l.goal);
        let stock = s.stock;
        let mut places = vec![Config::place(s)];
        for _ in 0..2 {
            s = fire(&w, &l, s);
            places.push(Config::place(s));
        }
        assert_eq!((s.hits, s.stock, s.goal), (2, stock - 2, false));
        assert_eq!(w.key(s, false).tier, 1);
        places.dedup();
        assert_eq!(places.len(), 3);
        let left = walk(&w, &l, s, l.farms[0]);
        assert_eq!(left.hits, 0);
        if s.stock == 0 {
            assert_eq!(fire(&w, &l, s), s);
            s = walk(&w, &l, left, l.farms[1]);
            while s.stock < 3 {
                s = walk(&w, &l, s, l.farms[0]);
                s = walk(&w, &l, s, l.farms[1]);
            }
            s = walk(&w, &l, s, l.goal);
            for _ in 0..2 {
                s = fire(&w, &l, s);
            }
        }
        s = fire(&w, &l, s);
        assert!(s.goal);
        assert!(w.goal(s));
    }

    #[test]
    fn a_boss_campaign_arrives_stocked_between_the_item_and_the_goal() {
        let workload = crate::Workload {
            config: crate::worlds::World::Map(Config {
                farms: 2,
                farm_cap: 3,
                boss_stock: 3,
                ..config(crate::test_seed())
            }),
            broken: false,
            scale: None,
        };
        let report = crate::run(&workload, crate::test_seed(), 200_000, true).unwrap();
        let goal = report["first_objective_work"].as_u64().unwrap();
        let tiers: Vec<Option<u64>> =
            serde_json::from_value(report["evidence"]["map_first_tier"].clone()).unwrap();
        let stocked = report["evidence"]["map_first_stocked"].as_u64().unwrap();
        assert!(tiers[1].unwrap() < stocked && stocked < goal);
    }

    #[test]
    fn a_boss_that_hits_back_needs_health_as_well_as_stock() {
        let w = Config {
            farms: 2,
            farm_cap: 3,
            boss_stock: 3,
            boss_hits_back: true,
            ..config(crate::test_seed())
        };
        let l = w.layout();
        assert!(w.reachable().unwrap());
        let armed = State {
            item: true,
            health: 2,
            ..w.initial()
        };
        let farmed = w.arrive(&l, armed, l.farms[1]);
        assert_eq!((farmed.stock, farmed.health), (1, 1));
        let healed = w.arrive(&l, farmed, l.farms[0]);
        assert_eq!((healed.stock, healed.health), (1, 2));
        let at_boss = |health| State {
            cell: l.goal,
            item: true,
            stock: 3,
            health,
            ..w.initial()
        };
        let mut s = at_boss(2);
        for _ in 0..2 {
            let before = s;
            s = fire(&w, &l, s);
            assert_eq!(
                (s.hits, s.stock, s.health),
                (before.hits + 1, before.stock - 1, before.health - 1)
            );
        }
        assert_eq!(fire(&w, &l, s), s);
        assert!(!s.goal);
        let mut s = at_boss(3);
        for _ in 0..3 {
            s = fire(&w, &l, s);
        }
        assert!(s.goal && w.goal(s));
    }

    #[test]
    fn a_locked_region_opens_with_a_key_from_the_far_end() {
        let w = Config {
            width: 8,
            height: 8,
            inner: 4,
            locked: true,
            ..config(crate::test_seed())
        };
        let l = w.layout();
        assert!(w.reachable().unwrap());
        let key = l.key.unwrap();
        assert!(!l.inner[usize::from(key)] && key != l.goal && key != l.door_cell);
        let mut shut = State {
            cell: l.door_cell,
            ..w.initial()
        };
        assert!(w.state_is_bounded(shut));
        for _ in 0..8 {
            shut = w.step(shut, l.door_direction);
        }
        assert_eq!(shut.cell, l.door_cell);
        let s = walk(&w, &l, w.initial(), key);
        assert_eq!((s.found, w.key(s, false).tier), (1, 1));
        let s = walk(&w, &l, s, l.item.unwrap());
        assert!(s.item);
        assert_eq!(w.key(s, false).tier, 2);
        let s = walk(&w, &l, s, l.goal);
        assert!(s.goal && w.goal(s));
        let early = walk(&w, &l, walk(&w, &l, w.initial(), key), l.goal);
        assert!(!early.goal);
    }

    #[test]
    fn an_optional_item_raises_the_tier_without_being_needed() {
        let w = Config {
            width: 8,
            height: 8,
            inner: 4,
            item_optional: true,
            ..config(crate::test_seed())
        };
        let l = w.layout();
        assert!(w.reachable().unwrap());
        assert_ne!(l.goal, l.door_cell);
        let plain = walk(&w, &l, w.initial(), l.goal);
        assert!(plain.goal && !plain.item);
        assert_eq!(w.key(plain, false).tier, 0);
        let item = walk(&w, &l, w.initial(), l.item.unwrap());
        assert_eq!(w.key(item, false).tier, 1);
        assert!(walk(&w, &l, item, l.goal).goal);
    }

    #[test]
    fn timing_rotates_actions_by_a_phase_the_key_does_not_see() {
        let w = Config {
            timing: 5,
            ..config(crate::test_seed())
        };
        let l = w.layout();
        assert!(w.reachable().unwrap());
        let plain = Config { timing: 0, ..w };
        let start = w.initial();
        for phase in 0..5 {
            let s = State { phase, ..start };
            assert!(w.state_is_bounded(s));
            assert_eq!(w.key(s, false), w.key(start, false));
            for action in 0..4 {
                let moved = w.step(s, action);
                let expected = plain.step(start, (action + phase) % 4);
                assert_eq!(moved.phase, (phase + 1) % 5);
                assert_eq!(State { phase: 0, ..moved }, expected);
            }
        }
        assert!(!w.state_is_bounded(State { phase: 5, ..start }));
        assert_eq!(*l, *plain.layout());
    }
}
