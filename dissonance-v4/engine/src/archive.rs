// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeMap, sync::Arc};

use serde::Serialize;

use crate::{
    fenwick::Fenwick,
    target::{Action, Kind},
};

pub type Key = Vec<u64>;

pub const FINE_LEVELS: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    Better,
    Worse,
    Equal,
    Incomparable,
}

pub fn compare(kinds: &[Kind], a: &[u64], b: &[u64]) -> Order {
    let mut better = false;
    let mut worse = false;
    for ((k, x), y) in kinds.iter().zip(a).zip(b) {
        let ord = match k {
            Kind::Identity => continue,
            Kind::Higher => x.cmp(y),
            Kind::Lower => y.cmp(x),
        };
        match ord {
            std::cmp::Ordering::Greater => better = true,
            std::cmp::Ordering::Less => worse = true,
            std::cmp::Ordering::Equal => {}
        }
    }
    match (better, worse) {
        (true, false) => Order::Better,
        (false, true) => Order::Worse,
        (false, false) => Order::Equal,
        (true, true) => Order::Incomparable,
    }
}

pub fn improves(
    kinds: &[Kind],
    scalars: &[u64],
    route: u64,
    holder: &[u64],
    holder_route: u64,
) -> bool {
    match compare(kinds, scalars, holder) {
        Order::Better | Order::Incomparable => true,
        Order::Equal => route < holder_route,
        Order::Worse => false,
    }
}

pub struct StateRec {
    pub parent: Option<u32>,
    pub actions: Vec<Action>,
    pub route: u64,
    pub found: u64,
    pub scalars: Vec<u64>,
    pub snapshot: Option<Arc<Vec<u8>>>,
    pub cell: u32,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CellRec {
    pub key: Key,
    pub front: Vec<u32>,
    pub chosen: u64,
    pub since_change: u64,
    pub empty_streak: u64,
    pub found: u64,
    pub first_parent_cell: Option<u32>,
    pub changes: u64,
    pub rank: u64,
    pub group: u32,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct GroupRec {
    pub key: Key,
    pub cells: Vec<u32>,
    pub chosen: u64,
    pub since_change: u64,
    pub rank: u64,
}

pub struct Archive {
    pub kinds: Vec<Kind>,
    pub shifts: Vec<u32>,
    pub states: Vec<StateRec>,
    pub cells: Vec<CellRec>,
    pub index: BTreeMap<Key, u32>,
    pub groups: Vec<GroupRec>,
    pub group_index: BTreeMap<Key, u32>,
    pub weights: Fenwick,
    pub edges: BTreeMap<(u32, u32), u64>,
    pub ladders: Vec<Vec<u64>>,
}

fn weight_of(since_change: u64, rank: u64) -> f64 {
    1.0 / ((since_change as f64) + 1.0).sqrt() / (rank as f64 + 1.0)
}

pub fn weight(cell: &CellRec) -> f64 {
    weight_of(cell.since_change, cell.rank)
}

pub fn group_weight(group: &GroupRec) -> f64 {
    weight_of(group.since_change, group.rank)
}

fn fine_shift(shift: u32) -> u32 {
    shift.saturating_sub(FINE_LEVELS)
}

fn oriented(kind: Kind, v: u64) -> Option<u64> {
    match kind {
        Kind::Identity => None,
        Kind::Higher => Some(v),
        Kind::Lower => Some(u64::MAX - v),
    }
}

impl Archive {
    pub fn new(kinds: Vec<Kind>) -> Self {
        let shifts = vec![0; kinds.len()];
        Self {
            kinds,
            shifts,
            states: Vec::new(),
            cells: Vec::new(),
            index: BTreeMap::new(),
            groups: Vec::new(),
            group_index: BTreeMap::new(),
            weights: Fenwick::default(),
            edges: BTreeMap::new(),
            ladders: Vec::new(),
        }
    }

    pub fn rank_of(&self, scalars: &[u64]) -> u64 {
        self.kinds
            .iter()
            .zip(scalars)
            .filter_map(|(k, v)| oriented(*k, *v))
            .zip(&self.ladders)
            .map(|(v, ladder)| ladder.partition_point(|&x| x > v) as u64)
            .min()
            .unwrap_or(0)
    }

    fn cell_rank(&self, c: usize) -> u64 {
        self.cells[c]
            .front
            .iter()
            .map(|&s| self.rank_of(&self.states[s as usize].scalars))
            .min()
            .unwrap_or(0)
    }

    pub fn refresh_ranks(&mut self) {
        let mut ladders: Vec<Vec<u64>> = Vec::new();
        for (i, k) in self.kinds.iter().enumerate() {
            if k.is_identity() {
                continue;
            }
            let mut ladder: Vec<u64> = self
                .cells
                .iter()
                .flat_map(|c| c.front.iter())
                .filter_map(|&s| oriented(*k, self.states[s as usize].scalars[i]))
                .collect();
            ladder.sort_unstable_by(|a, b| b.cmp(a));
            ladder.dedup();
            ladders.push(ladder);
        }
        self.ladders = ladders;
        for c in 0..self.cells.len() {
            self.cells[c].rank = self.cell_rank(c);
        }
        for g in 0..self.groups.len() {
            self.refresh_group(g);
        }
    }

    fn refresh_group(&mut self, g: usize) {
        let rank = self.groups[g]
            .cells
            .iter()
            .map(|&c| self.cells[c as usize].rank)
            .min()
            .unwrap_or(0);
        self.groups[g].rank = rank;
        let w = group_weight(&self.groups[g]);
        self.weights.set(g, w);
    }

    pub fn pick_cell(&self, g: u32, unit: f64) -> u32 {
        let members = &self.groups[g as usize].cells;
        let total: f64 = members
            .iter()
            .map(|&c| weight(&self.cells[c as usize]))
            .sum();
        let mut left = unit * total;
        for &c in members {
            left -= weight(&self.cells[c as usize]);
            if left < 0.0 {
                return c;
            }
        }
        members[members.len() - 1]
    }

    pub fn key(&self, scalars: &[u64]) -> Key {
        self.kinds
            .iter()
            .zip(scalars)
            .zip(&self.shifts)
            .filter(|((k, _), _)| k.is_identity())
            .map(|((_, v), s)| v >> fine_shift(*s))
            .collect()
    }

    fn group_key(&self, key: &Key) -> Key {
        self.kinds
            .iter()
            .zip(&self.shifts)
            .filter(|(k, _)| k.is_identity())
            .zip(key)
            .map(|((_, s), v)| v >> (s - fine_shift(*s)))
            .collect()
    }

    pub fn would_admit(&self, key: &Key, scalars: &[u64], route: u64) -> bool {
        let Some(&c) = self.index.get(key) else {
            return true;
        };
        self.cells[c as usize].front.iter().all(|&s| {
            let h = &self.states[s as usize];
            improves(&self.kinds, scalars, route, &h.scalars, h.route)
        })
    }

    fn cell_for(&mut self, key: Key, found: u64, parent_cell: Option<u32>) -> (u32, bool) {
        if let Some(&c) = self.index.get(&key) {
            return (c, false);
        }
        let c = self.cells.len() as u32;
        let gk = self.group_key(&key);
        let g = match self.group_index.get(&gk) {
            Some(&g) => g,
            None => {
                let g = self.groups.len() as u32;
                self.groups.push(GroupRec {
                    key: gk.clone(),
                    ..GroupRec::default()
                });
                self.group_index.insert(gk, g);
                self.weights.push(1.0);
                g
            }
        };
        self.groups[g as usize].cells.push(c);
        self.cells.push(CellRec {
            key: key.clone(),
            found,
            first_parent_cell: parent_cell,
            group: g,
            ..CellRec::default()
        });
        self.index.insert(key, c);
        (c, true)
    }

    pub fn admit(&mut self, mut rec: StateRec, key: Key) -> Option<(u32, bool)> {
        if !self.would_admit(&key, &rec.scalars, rec.route) {
            return None;
        }
        let parent_cell = rec.parent.map(|p| self.states[p as usize].cell);
        let (c, fresh) = self.cell_for(key, rec.found, parent_cell);
        let id = self.states.len() as u32;
        rec.cell = c;
        let kinds = &self.kinds;
        let states = &mut self.states;
        let cell = &mut self.cells[c as usize];
        cell.front.retain(|&s| {
            let h = &states[s as usize];
            let keep = matches!(
                compare(kinds, &h.scalars, &rec.scalars),
                Order::Better | Order::Incomparable
            ) || (compare(kinds, &h.scalars, &rec.scalars) == Order::Equal
                && h.route <= rec.route);
            if !keep {
                states[s as usize].snapshot = None;
            }
            keep
        });
        cell.front.push(id);
        cell.since_change = 0;
        cell.changes += 1;
        if let Some(pc) = parent_cell
            && pc != c
        {
            *self.edges.entry((pc, c)).or_insert(0) += 1;
        }
        self.states.push(rec);
        self.cells[c as usize].rank = self.cell_rank(c as usize);
        let g = self.cells[c as usize].group as usize;
        self.groups[g].since_change = 0;
        self.refresh_group(g);
        Some((id, fresh))
    }

    pub fn note_chosen(&mut self, c: u32) {
        let cell = &mut self.cells[c as usize];
        cell.chosen += 1;
        cell.since_change += 1;
        let g = cell.group as usize;
        let group = &mut self.groups[g];
        group.chosen += 1;
        group.since_change += 1;
        let w = group_weight(group);
        self.weights.set(g, w);
    }

    pub fn rekey(&mut self, shifts: Vec<u32>) {
        self.shifts = shifts;
        let mut held: Vec<u32> = self
            .cells
            .iter()
            .flat_map(|c| c.front.iter().copied())
            .collect();
        held.sort_unstable();
        let old = std::mem::take(&mut self.cells);
        self.index.clear();
        self.groups.clear();
        self.group_index.clear();
        self.weights = Fenwick::default();
        let mut remap: BTreeMap<u32, u32> = BTreeMap::new();
        for s in held {
            let key = self.key(&self.states[s as usize].scalars);
            let old_cell = &old[self.states[s as usize].cell as usize];
            let (c, _) = self.cell_for(key, old_cell.found, None);
            remap.insert(self.states[s as usize].cell, c);
            let kinds = &self.kinds;
            let states = &mut self.states;
            let cell = &mut self.cells[c as usize];
            cell.found = cell.found.min(old_cell.found);
            let rec_scalars = states[s as usize].scalars.clone();
            let rec_route = states[s as usize].route;
            let dominated = cell.front.iter().any(|&h| {
                let hs = &states[h as usize];
                !improves(kinds, &rec_scalars, rec_route, &hs.scalars, hs.route)
            });
            if dominated {
                states[s as usize].snapshot = None;
                continue;
            }
            cell.front.retain(|&h| {
                let hs = &states[h as usize];
                let keep = improves(kinds, &hs.scalars, hs.route, &rec_scalars, rec_route);
                if !keep {
                    states[h as usize].snapshot = None;
                }
                keep
            });
            cell.front.push(s);
        }
        for s in 0..self.states.len() {
            let old_cell = self.states[s].cell;
            let key = self.key(&self.states[s].scalars);
            if let Some(&c) = self.index.get(&key) {
                self.states[s].cell = c;
            } else if let Some(&c) = remap.get(&old_cell) {
                self.states[s].cell = c;
            }
        }
        let old_edges = std::mem::take(&mut self.edges);
        for ((a, b), n) in old_edges {
            if let (Some(&x), Some(&y)) = (remap.get(&a), remap.get(&b))
                && x != y
            {
                *self.edges.entry((x, y)).or_insert(0) += n;
            }
        }
        for cell in &mut self.cells {
            if let Some(pc) = cell.first_parent_cell {
                cell.first_parent_cell = remap.get(&pc).copied();
            }
        }
    }

    pub fn holders(&self) -> usize {
        self.cells.iter().map(|c| c.front.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(parent: Option<u32>, scalars: Vec<u64>, route: u64) -> StateRec {
        StateRec {
            parent,
            actions: Vec::new(),
            route,
            found: route,
            scalars,
            snapshot: Some(Arc::new(vec![0])),
            cell: 0,
        }
    }

    #[test]
    fn progress_replaces_and_tradeoffs_coexist() {
        let kinds = vec![Kind::Identity, Kind::Higher, Kind::Lower];
        let mut a = Archive::new(kinds);
        let k = a.key(&[5, 0, 0]);
        assert!(a.admit(rec(None, vec![5, 3, 3], 10), k.clone()).is_some());
        assert!(
            a.admit(rec(Some(0), vec![5, 3, 3], 20), k.clone())
                .is_none()
        );
        assert!(a.admit(rec(Some(0), vec![5, 3, 3], 5), k.clone()).is_some());
        assert_eq!(a.cells[0].front, vec![1]);
        assert!(
            a.admit(rec(Some(0), vec![5, 4, 4], 50), k.clone())
                .is_some()
        );
        assert_eq!(a.cells[0].front, vec![1, 2]);
        assert!(
            a.admit(rec(Some(0), vec![5, 4, 2], 60), k.clone())
                .is_some()
        );
        assert_eq!(a.cells[0].front, vec![3]);
        assert!(a.states[1].snapshot.is_none());
        assert!(
            a.admit(rec(Some(3), vec![6, 0, 9], 70), a.key(&[6, 0, 9]))
                .is_some()
        );
        assert_eq!(a.edges.get(&(0, 1)), Some(&1));
    }

    #[test]
    fn rekey_merges_cells_and_keeps_best() {
        let kinds = vec![Kind::Identity, Kind::Higher];
        let mut a = Archive::new(kinds);
        for (x, p) in [(0u64, 1u64), (1, 3), (2, 2), (9, 0)] {
            let k = a.key(&[x, p]);
            a.admit(rec(None, vec![x, p], x), k).unwrap();
        }
        assert_eq!(a.cells.len(), 4);
        a.rekey(vec![2 + FINE_LEVELS, 0]);
        assert_eq!(a.cells.len(), 2);
        let c0 = a.index[&vec![0]];
        assert_eq!(a.cells[c0 as usize].front, vec![1]);
        assert_eq!(a.holders(), 2);
        assert_eq!(a.groups.len(), 1);
        assert_eq!(a.groups[0].cells.len(), 2);
    }

    #[test]
    fn selection_weighs_coarse_groups_then_fine_cells() {
        let kinds = vec![Kind::Identity];
        let mut a = Archive::new(kinds);
        a.shifts = vec![FINE_LEVELS + 1];
        for x in [0u64, 2, 4, 6, 8] {
            let k = a.key(&[x]);
            a.admit(rec(None, vec![x], x), k).unwrap();
        }
        assert_eq!(a.cells.len(), 5);
        assert_eq!(a.groups.len(), 2);
        assert_eq!(a.weights.total(), 2.0);
        for _ in 0..3 {
            a.note_chosen(0);
        }
        assert_eq!(a.groups[0].since_change, 3);
        assert_eq!(a.pick_cell(0, 0.1), 0);
        assert_eq!(a.pick_cell(0, 0.2), 1);
        assert_eq!(a.pick_cell(1, 0.5), 4);
    }

    #[test]
    fn rank_counts_distinct_better_progress() {
        let kinds = vec![Kind::Identity, Kind::Higher, Kind::Lower];
        let mut a = Archive::new(kinds);
        for (x, h, l) in [(0u64, 1u64, 9u64), (1, 5, 9), (2, 5, 9), (3, 2, 1)] {
            let k = a.key(&[x, h, l]);
            a.admit(rec(None, vec![x, h, l], x), k).unwrap();
        }
        a.refresh_ranks();
        let ranks: Vec<u64> = a.cells.iter().map(|c| c.rank).collect();
        assert_eq!(ranks, vec![1, 0, 0, 0]);
        assert_eq!(a.rank_of(&[7, 0, 9]), 1);
        assert_eq!(a.rank_of(&[7, 9, 9]), 0);
        let untouched = Archive::new(vec![Kind::Identity]);
        assert_eq!(untouched.rank_of(&[3]), 0);
    }
}
