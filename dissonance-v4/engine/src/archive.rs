// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeMap, sync::Arc};

use serde::Serialize;

use crate::{
    fenwick::Fenwick,
    snap::Snap,
    strata::Strata,
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

pub struct StateRec {
    pub parent: Option<u32>,
    pub actions: Vec<Action>,
    pub route: u64,
    pub found: u64,
    pub scalars: Vec<u64>,
    pub snapshot: Option<Arc<Snap>>,
    pub cell: u32,
    pub job: u64,
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
    pub rarity: f64,
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
    pub strata: Strata,
    pub edges: BTreeMap<(u32, u32), u64>,
    pub ladders: Vec<Vec<u64>>,
    pub next_prune: usize,
    pub value_counts: Vec<BTreeMap<u64, u64>>,
    pub value_medians: Vec<u64>,
}

fn weight_of(since_change: u64, rank: u64) -> f64 {
    1.0 / ((since_change as f64) + 1.0).sqrt() / (rank as f64 + 1.0)
}

pub fn weight(cell: &CellRec) -> f64 {
    weight_of(cell.since_change, cell.rank)
}

pub fn group_weight(group: &GroupRec) -> f64 {
    weight_of(group.since_change, group.rank) * group.rarity
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
            strata: Strata::default(),
            edges: BTreeMap::new(),
            ladders: Vec::new(),
            next_prune: 0,
            value_counts: Vec::new(),
            value_medians: Vec::new(),
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
        self.refresh_rarity();
        for c in 0..self.cells.len() {
            self.cells[c].rank = self.cell_rank(c);
        }
        for g in 0..self.groups.len() {
            self.refresh_group(g);
        }
    }

    fn rarity_of(&self, key: &Key) -> f64 {
        key.iter()
            .zip(&self.value_counts)
            .zip(&self.value_medians)
            .map(|((v, counts), &median)| {
                let n = counts.get(v).copied().unwrap_or(0).max(1);
                (median as f64 / n as f64).sqrt()
            })
            .fold(1.0, f64::max)
    }

    fn refresh_rarity(&mut self) {
        let width = self.groups.first().map_or(0, |g| g.key.len());
        let mut counts: Vec<BTreeMap<u64, u64>> = vec![BTreeMap::new(); width];
        for g in &self.groups {
            for (m, v) in counts.iter_mut().zip(&g.key) {
                *m.entry(*v).or_insert(0) += 1;
            }
        }
        self.value_medians = counts
            .iter()
            .map(|m| {
                let mut n: Vec<u64> = m.values().copied().collect();
                n.sort_unstable();
                n.get(n.len() / 2).copied().unwrap_or(1)
            })
            .collect();
        self.value_counts = counts;
        for g in 0..self.groups.len() {
            self.groups[g].rarity = self.rarity_of(&self.groups[g].key);
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
        self.strata.set(g, w);
    }

    pub fn pick_group(&self, scalar: u64, unit_value: f64, unit_group: f64) -> u32 {
        if self.strata.width() == 0 {
            return self.weights.find(unit_value * self.weights.total()) as u32;
        }
        self.strata.pick(scalar as usize, unit_value, unit_group)
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
        let mut shortest = u64::MAX;
        let mut longest = 0;
        let mut equal = false;
        for &s in &self.cells[c as usize].front {
            let h = &self.states[s as usize];
            match compare(&self.kinds, scalars, &h.scalars) {
                Order::Worse => return false,
                Order::Equal => {
                    equal = true;
                    shortest = shortest.min(h.route);
                    longest = longest.max(h.route);
                }
                Order::Better | Order::Incomparable => {}
            }
        }
        !equal || route < shortest || route > longest
    }

    fn reduce(&mut self, members: Vec<u32>) -> Vec<u32> {
        let kinds = &self.kinds;
        let states = &self.states;
        let keep: Vec<bool> = members
            .iter()
            .map(|&m| {
                let x = &states[m as usize];
                let mut first = true;
                let mut last = true;
                for &o in &members {
                    if o == m {
                        continue;
                    }
                    let y = &states[o as usize];
                    match compare(kinds, &y.scalars, &x.scalars) {
                        Order::Better => return false,
                        Order::Equal => {
                            if (y.route, o) < (x.route, m) {
                                first = false;
                            }
                            if (y.route, std::cmp::Reverse(o)) > (x.route, std::cmp::Reverse(m)) {
                                last = false;
                            }
                        }
                        Order::Worse | Order::Incomparable => {}
                    }
                }
                first || last
            })
            .collect();
        let mut kept = Vec::new();
        for (m, k) in members.into_iter().zip(keep) {
            if k {
                kept.push(m);
            } else {
                self.states[m as usize].snapshot = None;
            }
        }
        kept
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
                for (m, v) in self.value_counts.iter_mut().zip(&gk) {
                    *m.entry(*v).or_insert(0) += 1;
                }
                let rarity = self.rarity_of(&gk);
                self.groups.push(GroupRec {
                    key: gk.clone(),
                    rarity,
                    ..GroupRec::default()
                });
                self.strata.push(&gk, 1.0);
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
        let equal = self.cells[c as usize].front.iter().any(|&h| {
            matches!(
                compare(&self.kinds, &self.states[h as usize].scalars, &rec.scalars),
                Order::Equal
            )
        });
        let id = self.states.len() as u32;
        rec.cell = c;
        self.states.push(rec);
        let mut members = std::mem::take(&mut self.cells[c as usize].front);
        members.push(id);
        let kept = self.reduce(members);
        let cell = &mut self.cells[c as usize];
        cell.front = kept;
        if !equal {
            cell.since_change = 0;
        }
        cell.changes += 1;
        if let Some(pc) = parent_cell
            && pc != c
        {
            *self.edges.entry((pc, c)).or_insert(0) += 1;
        }
        self.cells[c as usize].rank = self.cell_rank(c as usize);
        let g = self.cells[c as usize].group as usize;
        if !equal {
            self.groups[g].since_change = 0;
        }
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
        self.strata.set(g, w);
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
        self.strata.clear();
        let mut remap: BTreeMap<u32, u32> = BTreeMap::new();
        for s in held {
            let key = self.key(&self.states[s as usize].scalars);
            let old_cell = &old[self.states[s as usize].cell as usize];
            let (c, _) = self.cell_for(key, old_cell.found, None);
            remap.insert(self.states[s as usize].cell, c);
            let cell = &mut self.cells[c as usize];
            cell.found = cell.found.min(old_cell.found);
            cell.empty_streak = cell.empty_streak.max(old_cell.empty_streak);
            cell.front.push(s);
        }
        for c in 0..self.cells.len() {
            let members = std::mem::take(&mut self.cells[c].front);
            self.cells[c].front = self.reduce(members);
        }
        for s in 0..self.states.len() {
            if self.states[s].scalars.is_empty() && !self.kinds.is_empty() {
                continue;
            }
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

    pub fn maybe_prune(&mut self) {
        if self.states.len() >= self.next_prune {
            let live = self.prune_routes();
            self.next_prune = self.states.len() + live.max(1);
        }
    }

    pub fn prune_routes(&mut self) -> usize {
        let mut live = vec![false; self.states.len()];
        for cell in &self.cells {
            for &h in &cell.front {
                let mut at = Some(h);
                while let Some(s) = at {
                    if live[s as usize] {
                        break;
                    }
                    live[s as usize] = true;
                    at = self.states[s as usize].parent;
                }
            }
        }
        let mut kept = 0;
        for (rec, keep) in self.states.iter_mut().zip(live) {
            if keep {
                kept += 1;
            } else {
                rec.actions = Vec::new();
                rec.scalars = Vec::new();
                rec.snapshot = None;
            }
        }
        kept
    }

    pub fn holders(&self) -> usize {
        self.cells.iter().map(|c| c.front.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn rec(parent: Option<u32>, scalars: Vec<u64>, route: u64) -> StateRec {
        StateRec {
            parent,
            actions: Vec::new(),
            route,
            found: route,
            scalars,
            snapshot: Some(Arc::new(Snap::full(&[0]))),
            cell: 0,
            job: 0,
        }
    }

    #[test]
    fn progress_replaces_and_tradeoffs_coexist() {
        let kinds = vec![Kind::Identity, Kind::Higher, Kind::Lower];
        let mut a = Archive::new(kinds);
        let k = a.key(&[5, 0, 0]);
        assert!(a.admit(rec(None, vec![5, 3, 3], 10), k.clone()).is_some());
        assert!(
            a.admit(rec(Some(0), vec![5, 3, 4], 20), k.clone())
                .is_none()
        );
        assert!(a.admit(rec(Some(0), vec![5, 3, 3], 5), k.clone()).is_some());
        assert_eq!(a.cells[0].front, vec![0, 1]);
        assert!(
            a.admit(rec(Some(0), vec![5, 4, 4], 50), k.clone())
                .is_some()
        );
        assert_eq!(a.cells[0].front, vec![0, 1, 2]);
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
        for (c, e) in [(0usize, 3u64), (1, 7), (2, 5), (3, 2)] {
            a.cells[c].empty_streak = e;
        }
        a.rekey(vec![2 + FINE_LEVELS, 0]);
        assert_eq!(a.cells.len(), 2);
        let c0 = a.index[&vec![0]];
        assert_eq!(a.cells[c0 as usize].front, vec![1]);
        assert_eq!(a.cells[c0 as usize].empty_streak, 7);
        assert_eq!(a.cells[a.index[&vec![2]] as usize].empty_streak, 2);
        assert_eq!(a.holders(), 2);
        assert_eq!(a.groups.len(), 1);
        assert_eq!(a.groups[0].cells.len(), 2);
    }

    #[test]
    fn prune_keeps_holders_and_their_ancestors() {
        let kinds = vec![Kind::Identity, Kind::Higher];
        let mut a = Archive::new(kinds);
        let step = Action {
            input: 1,
            arg: 0,
            hold: 1,
        };
        let add = |a: &mut Archive, parent, x: u64, p: u64| {
            let mut r = rec(parent, vec![x, p], x);
            r.actions = vec![step];
            let k = a.key(&[x, p]);
            a.admit(r, k).unwrap().0
        };
        let root = add(&mut a, None, 0, 0);
        let mid = add(&mut a, Some(root), 1, 0);
        let leaf = add(&mut a, Some(mid), 2, 0);
        let mid2 = add(&mut a, Some(root), 1, 5);
        let dead = add(&mut a, Some(mid2), 3, 0);
        let better = add(&mut a, Some(root), 1, 9);
        a.admit(rec(Some(root), vec![3, 1], 3), a.key(&[3, 1]))
            .unwrap();
        a.prune_routes();
        for s in [root, mid, leaf, better] {
            assert_eq!(a.states[s as usize].actions.len(), 1, "{s}");
            assert_eq!(a.states[s as usize].scalars.len(), 2, "{s}");
        }
        for s in [mid2, dead] {
            assert!(a.states[s as usize].actions.is_empty(), "{s}");
            assert!(a.states[s as usize].snapshot.is_none(), "{s}");
        }
        a.rekey(vec![2 + FINE_LEVELS, 0]);
        assert_eq!(a.holders(), 1);
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
    fn equal_progress_arrival_keeps_the_choice_counts() {
        let kinds = vec![Kind::Identity, Kind::Higher];
        let mut a = Archive::new(kinds);
        let k = a.key(&[0, 1]);
        a.admit(rec(None, vec![0, 1], 5), k.clone()).unwrap();
        for _ in 0..3 {
            a.note_chosen(0);
        }
        a.admit(rec(Some(0), vec![0, 1], 9), k.clone()).unwrap();
        assert_eq!(a.cells[0].front.len(), 2);
        assert_eq!(a.cells[0].since_change, 3);
        assert_eq!(a.groups[0].since_change, 3);
        a.admit(rec(Some(0), vec![0, 2], 9), k).unwrap();
        assert_eq!(a.cells[0].since_change, 0);
        assert_eq!(a.groups[0].since_change, 0);
    }

    #[test]
    fn group_draw_gives_each_identity_value_its_own_share() {
        let kinds = vec![Kind::Identity, Kind::Identity];
        let mut a = Archive::new(kinds);
        for x in 0..9u64 {
            let k = a.key(&[0, x]);
            a.admit(rec(None, vec![0, x], x), k).unwrap();
        }
        let k = a.key(&[1, 100]);
        a.admit(rec(None, vec![1, 100], 9), k).unwrap();
        a.refresh_ranks();
        let lone = a.group_index[&vec![1, 100]];
        let share = |a: &Archive, s: u64| {
            (0..1000)
                .filter(|&i| a.pick_group(s, (f64::from(i) + 0.5) / 1000.0, 0.5) == lone)
                .count()
        };
        assert_eq!(share(&a, 0), 750);
        assert_eq!(share(&a, 1), 250);
        let crowd: BTreeSet<u32> = (0..1000)
            .map(|i| a.pick_group(0, 0.1, (f64::from(i) + 0.5) / 1000.0))
            .collect();
        assert_eq!(crowd.len(), 9);
        assert!(!crowd.contains(&lone));
        a.rekey(vec![0, 0]);
        a.refresh_ranks();
        let lone = a.group_index[&vec![1, 100]];
        assert_eq!(
            (0..1000)
                .filter(|&i| a.pick_group(0, (f64::from(i) + 0.5) / 1000.0, 0.5) == lone)
                .count(),
            750
        );
    }

    #[test]
    fn rare_identity_values_raise_group_weight() {
        let kinds = vec![Kind::Identity, Kind::Identity];
        let mut a = Archive::new(kinds);
        for (x, e) in [(0u64, 0u64), (1, 0), (2, 0), (3, 0), (0, 1)] {
            let k = a.key(&[x, e]);
            a.admit(rec(None, vec![x, e], x), k).unwrap();
        }
        a.refresh_ranks();
        let g = |a: &Archive, k: Vec<u64>| a.group_index[&k] as usize;
        assert_eq!(a.groups[g(&a, vec![0, 1])].rarity, 2.0);
        assert_eq!(a.groups[g(&a, vec![1, 0])].rarity, 1.0);
        assert_eq!(group_weight(&a.groups[g(&a, vec![0, 1])]), 2.0);
        assert_eq!(group_weight(&a.groups[g(&a, vec![1, 0])]), 1.0);
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

    #[test]
    fn equal_progress_keeps_the_shortest_and_the_longest_route() {
        let kinds = vec![Kind::Identity, Kind::Higher];
        let mut a = Archive::new(kinds);
        let k = a.key(&[5, 3]);
        a.admit(rec(None, vec![5, 3], 10), k.clone()).unwrap();
        assert!(a.would_admit(&k, &[5, 3], 20));
        a.admit(rec(Some(0), vec![5, 3], 20), k.clone()).unwrap();
        assert_eq!(a.cells[0].front, vec![0, 1]);
        assert!(!a.would_admit(&k, &[5, 3], 15));
        assert!(a.admit(rec(Some(0), vec![5, 3], 15), k.clone()).is_none());
        a.admit(rec(Some(0), vec![5, 3], 30), k.clone()).unwrap();
        assert_eq!(a.cells[0].front, vec![0, 2]);
        assert!(a.states[1].snapshot.is_none());
        a.admit(rec(Some(0), vec![5, 3], 4), k.clone()).unwrap();
        assert_eq!(a.cells[0].front, vec![2, 3]);
        a.admit(rec(Some(0), vec![5, 4], 50), k.clone()).unwrap();
        assert_eq!(a.cells[0].front, vec![4]);
        a.admit(rec(Some(0), vec![9, 1], 1), a.key(&[9, 1]))
            .unwrap();
        a.admit(rec(Some(0), vec![9, 1], 70), a.key(&[9, 1]))
            .unwrap();
        a.rekey(vec![4 + FINE_LEVELS, 0]);
        let c = a.index[&vec![0]] as usize;
        assert_eq!(a.cells[c].front, vec![4]);
        a.admit(rec(Some(0), vec![9, 4], 2), a.key(&[9, 4]))
            .unwrap();
        a.admit(rec(Some(0), vec![9, 4], 90), a.key(&[9, 4]))
            .unwrap();
        assert_eq!(a.cells[c].front, vec![7, 8]);
        assert!(a.states[4].snapshot.is_none());
    }
}
