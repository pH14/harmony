// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::Key;
use searcher::search::rand::RomuDuoJrRand;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::rc::Rc;

const SUBPLACES: u16 = 16;

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
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub cell: u8,
    pub arm: u8,
    pub progress: u8,
    pub item: bool,
    pub goal: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Layout {
    pub doors: Vec<u8>,
    pub inner: Vec<bool>,
    pub door_cell: u8,
    pub door_direction: u8,
    pub item: u8,
    pub goal: u8,
    pub start_to_door: u8,
    pub door_to_item: u8,
    pub door_to_goal: u8,
    pub loops: u8,
}

thread_local! {
    static LAYOUT: RefCell<Option<(Config, Rc<Layout>)>> = const { RefCell::new(None) };
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
        let entry = (1..self.cells())
            .filter(|&cell| subtree[usize::from(cell)] <= self.cells() - 2)
            .min_by_key(|&cell| (subtree[usize::from(cell)].abs_diff(self.inner), cell))
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
        let item = (0..self.cells())
            .filter(|&c| inner[usize::from(c)])
            .max_by_key(|&c| (from_entry[usize::from(c)], std::cmp::Reverse(c)))
            .expect("inner rooms");
        let from_door = self.distances(&doors, door_cell);
        let goal = (0..self.cells())
            .filter(|&c| !inner[usize::from(c)])
            .max_by_key(|&c| (from_door[usize::from(c)], std::cmp::Reverse(c)))
            .expect("outer rooms");
        Layout {
            start_to_door: from_door[0],
            door_to_item: from_entry[usize::from(item)] + 1,
            door_to_goal: from_door[usize::from(goal)],
            doors,
            inner,
            door_cell,
            door_direction,
            item,
            goal,
            loops: added,
        }
    }

    pub fn initial(&self) -> State {
        State {
            cell: 0,
            arm: 0,
            progress: 0,
            item: false,
            goal: false,
        }
    }

    fn state_fits(&self, layout: &Layout, s: State) -> bool {
        if s.cell >= self.cells() || s.arm > 4 {
            return false;
        }
        let arm_fits = if s.arm == 0 {
            s.progress == 0
        } else {
            let direction = s.arm - 1;
            layout.doors[usize::from(s.cell)] & (1 << direction) != 0
                && (1..self.length(direction)).contains(&s.progress)
        };
        arm_fits
            && (s.item || s.cell != layout.item)
            && s.goal == (s.item && s.arm == 0 && s.cell == layout.goal)
    }

    pub fn state_is_bounded(&self, s: State) -> bool {
        self.validate().is_ok() && self.state_fits(&self.layout(), s)
    }

    pub fn goal(&self, s: State) -> bool {
        self.state_is_bounded(s) && s.goal
    }

    fn arrive(&self, layout: &Layout, s: State, cell: u8) -> State {
        let item = s.item || cell == layout.item;
        State {
            cell,
            arm: 0,
            progress: 0,
            item,
            goal: item && cell == layout.goal,
        }
    }

    pub fn step(&self, s: State, action: u8) -> State {
        let layout = self.layout();
        if action > 3 || !self.state_fits(&layout, s) || s.goal {
            return s;
        }
        if s.arm == 0 {
            if layout.doors[usize::from(s.cell)] & (1 << action) == 0 {
                return s;
            }
            if self.length(action) == 1 {
                let next = self.neighbour(s.cell, action).expect("door has a room");
                return self.arrive(&layout, s, next);
            }
            return State {
                arm: action + 1,
                progress: 1,
                ..s
            };
        }
        let direction = s.arm - 1;
        if action == direction {
            if s.progress + 1 == self.length(direction) {
                let next = self.neighbour(s.cell, direction).expect("door has a room");
                return self.arrive(&layout, s, next);
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
        let sub = if s.arm == 0 {
            0
        } else {
            1 + u16::from(s.arm - 1) * 3 + u16::from(s.progress - 1)
        };
        u16::from(s.cell) * SUBPLACES + sub
    }

    pub fn key(&self, s: State, broken: bool) -> Key {
        Key {
            stock: 0,
            place: Self::place(s),
            context: 0,
            charge: 0,
            health: 0,
            goal: s.goal,
            tier: u16::from(s.item && !broken),
        }
    }

    pub fn reachable(&self) -> Result<bool, String> {
        self.validate()?;
        crate::reachable(self.initial(), |s| self.goal(s), |s, a| self.step(s, a))
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
            assert!(!l.inner[0] && l.inner[usize::from(l.item)] && !l.inner[usize::from(l.goal)]);
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
            assert!(l.door_to_item > 0 && l.door_to_goal > 0);
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
        let with_item = walk(&w, &l, early, l.item);
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
        for cell in (0..w.cells()).filter(|&c| c != l.item) {
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
            let timeline: Vec<[u64; 3]> =
                serde_json::from_value(report["parent_timeline"].clone()).unwrap();
            assert!(timeline.windows(2).all(|w| w[0][0] <= w[1][0]));
            assert!(timeline.iter().all(|t| t[1] <= 1));
        }
    }

    #[test]
    fn states_outside_the_layout_are_rejected() {
        let w = config(5);
        let l = w.layout();
        assert!(!w.state_is_bounded(State {
            cell: l.item,
            ..w.initial()
        }));
        let direction = (0..4)
            .find(|&d| l.doors[usize::from(l.item)] & (1 << d) != 0 && w.length(d) > 1)
            .unwrap();
        assert!(!w.state_is_bounded(State {
            cell: l.item,
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
}
