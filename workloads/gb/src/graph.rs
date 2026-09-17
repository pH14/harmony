// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::target::{
    CUR_MAP, CUR_MAP_CONNECTIONS, EAST_CONNECTION_MAP, LAST_MAP, MAX_WARPS, NORTH_CONNECTION_MAP,
    NUMBER_OF_WARPS, SOUTH_CONNECTION_MAP, WARP_DESTINATION_MAP, WARP_ENTRIES, WARP_ENTRY_BYTES,
    WEST_CONNECTION_MAP, byte,
};

const CONNECTION_HEADERS: [(u8, u16); 4] = [
    (3, NORTH_CONNECTION_MAP),
    (2, SOUTH_CONNECTION_MAP),
    (1, WEST_CONNECTION_MAP),
    (0, EAST_CONNECTION_MAP),
];

#[must_use]
pub fn map_links(wram: &[u8]) -> (u8, Vec<u8>) {
    let here = byte(wram, CUR_MAP);
    let connections = byte(wram, CUR_MAP_CONNECTIONS);
    let mut links = Vec::new();
    for (bit, address) in CONNECTION_HEADERS {
        if connections & (1 << bit) != 0 {
            links.push(byte(wram, address));
        }
    }
    let warps = byte(wram, NUMBER_OF_WARPS).min(MAX_WARPS);
    for index in 0..u16::from(warps) {
        let base = WARP_ENTRIES + WARP_ENTRY_BYTES * index;
        let destination = byte(wram, base + WARP_DESTINATION_MAP);
        if destination != LAST_MAP {
            links.push(destination);
        }
    }
    links.sort_unstable();
    links.dedup();
    links.retain(|map| *map != here);
    (here, links)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MapGraph {
    edges: BTreeMap<u8, BTreeSet<u8>>,
}

impl MapGraph {
    pub fn record(&mut self, here: u8, links: &[u8]) {
        self.edges.entry(here).or_default();
        for link in links {
            self.edges.entry(here).or_default().insert(*link);
            self.edges.entry(*link).or_default().insert(here);
        }
    }

    pub fn observe(&mut self, wram: &[u8]) {
        let (here, links) = map_links(wram);
        self.record(here, &links);
    }

    #[must_use]
    pub fn maps(&self) -> usize {
        self.edges.len()
    }

    #[must_use]
    pub fn neighbours(&self, map: u8) -> Option<&BTreeSet<u8>> {
        self.edges.get(&map)
    }

    #[must_use]
    pub fn hops_to(&self, target: u8) -> BTreeMap<u8, usize> {
        let mut hops = BTreeMap::new();
        if !self.edges.contains_key(&target) {
            return hops;
        }
        hops.insert(target, 0);
        let mut queue = VecDeque::from([target]);
        while let Some(map) = queue.pop_front() {
            let distance = hops[&map];
            let Some(neighbours) = self.edges.get(&map) else {
                continue;
            };
            for neighbour in neighbours {
                if !hops.contains_key(neighbour) {
                    hops.insert(*neighbour, distance + 1);
                    queue.push_back(*neighbour);
                }
            }
        }
        hops
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::{CUR_MAP, WRAM_BASE};

    fn wram_with(pairs: &[(u16, u8)]) -> Vec<u8> {
        let mut wram = vec![0; 0x2000];
        for (address, value) in pairs {
            wram[usize::from(*address - WRAM_BASE)] = *value;
        }
        wram
    }

    #[test]
    fn links_read_both_the_set_connections_and_the_warp_destinations() {
        let wram = wram_with(&[
            (CUR_MAP, 0),
            (CUR_MAP_CONNECTIONS, 0b1000),
            (NORTH_CONNECTION_MAP, 12),
            (SOUTH_CONNECTION_MAP, 99),
            (NUMBER_OF_WARPS, 3),
            (WARP_ENTRIES + WARP_DESTINATION_MAP, 40),
            (WARP_ENTRIES + WARP_ENTRY_BYTES + WARP_DESTINATION_MAP, 37),
            (
                WARP_ENTRIES + WARP_ENTRY_BYTES * 2 + WARP_DESTINATION_MAP,
                LAST_MAP,
            ),
        ]);
        let (here, links) = map_links(&wram);
        assert_eq!(here, 0);
        assert_eq!(
            links,
            vec![12, 37, 40],
            "an unset connection and the last-map sentinel are both skipped"
        );
    }

    #[test]
    fn hops_reach_a_neighbour_no_map_table_was_read_from() {
        let mut graph = MapGraph::default();
        graph.record(0, &[12, 40]);
        graph.record(12, &[0, 1]);
        let hops = graph.hops_to(40);
        assert_eq!(hops.get(&40), Some(&0));
        assert_eq!(hops.get(&0), Some(&1));
        assert_eq!(hops.get(&12), Some(&2));
        assert_eq!(
            hops.get(&1),
            Some(&3),
            "a map named only as a neighbour still gets a distance"
        );
        assert_eq!(graph.hops_to(200), BTreeMap::new());
    }
}
