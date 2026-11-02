# SPDX-License-Identifier: AGPL-3.0-or-later
"""Cluster a recorded archive graph into regions and score them against rooms.

Input is the JSON-lines file written by `nes-hindsight dump`: one line per
retained entry, `[sequence, id, parent_id, key]`. Checkpoints are the archive
just before each new progress tier first arrives.

The graph has one node per position (place, identity) and one undirected edge
per parent and child position pair, weighted by the number of retained
children. Regions come from Louvain modularity clustering with a fixed seed.
"""

from __future__ import annotations

import argparse
import json
import math
import random
import sys
from collections import Counter, defaultdict
from dataclasses import dataclass
from heapq import heappop, heappush

PERMUTATIONS = 20
SEED = 0


def frozen(value):
    if isinstance(value, list):
        return tuple(frozen(item) for item in value)
    if isinstance(value, dict):
        return tuple(sorted((name, frozen(item)) for name, item in value.items()))
    return value


@dataclass(frozen=True)
class Entry:
    sequence: int
    id: int
    parent: int
    key: dict
    progress: tuple
    place: tuple
    identity: tuple


@dataclass(frozen=True)
class Game:
    name: str

    def identity(self, entry: Entry, coarsen: str = ""):
        if self.name == "metroid":
            x, y, posture, door = entry.identity
            if coarsen == "posture":
                posture = 0
            elif coarsen == "door":
                door = 0
            elif coarsen == "bucket32":
                x, y = x // 2, y // 2
            elif coarsen == "bucket64":
                x, y = x // 4, y // 4
            return (x, y, posture, door)
        bucket = entry.identity
        if coarsen == "bucket32":
            bucket //= 2
        elif coarsen == "bucket64":
            bucket //= 4
        return bucket

    def room(self, entry: Entry) -> tuple:
        if self.name == "metroid":
            return entry.place[:3]
        return entry.progress[:2] + (entry.place[0],)

    def area(self, entry: Entry) -> tuple:
        if self.name == "metroid":
            return entry.place[:1]
        return entry.progress[:2]

    def preferences(self, entry: Entry) -> list[tuple]:
        key = entry.key
        if self.name == "metroid":
            return [
                (key["items"], key["tanks"], key["missiles"], key["health"]),
                (key["items"], key["tanks"], key["health"], key["missiles"]),
            ]
        return [(key["clock"],)]

    def coarsenings(self) -> list[str]:
        if self.name == "metroid":
            return ["posture", "door", "bucket32", "bucket64"]
        return ["bucket32", "bucket64"]


def louvain(nodes: list, adjacency: dict, rng: random.Random) -> dict:
    community = {node: node for node in nodes}
    members = {node: [node] for node in nodes}
    graph = {node: dict(adjacency.get(node, {})) for node in nodes}
    while True:
        level, moved = one_level(graph, rng)
        if not moved:
            break
        grouped: dict = defaultdict(list)
        for node, label in level.items():
            grouped[label].extend(members[node])
        members = dict(grouped)
        aggregated: dict = defaultdict(lambda: defaultdict(float))
        for node, neighbours in graph.items():
            for other, weight in neighbours.items():
                aggregated[level[node]][level[other]] += weight
        graph = {label: dict(neighbours) for label, neighbours in aggregated.items()}
        for label in members:
            graph.setdefault(label, {})
    for label, nodes_in in members.items():
        for node in nodes_in:
            community[node] = label
    return community


def one_level(graph: dict, rng: random.Random) -> tuple[dict, bool]:
    degree = {node: sum(neighbours.values()) for node, neighbours in graph.items()}
    total = sum(degree.values())
    if total == 0:
        return {node: node for node in graph}, False
    label = {node: node for node in graph}
    label_degree = dict(degree)
    order = list(graph)
    rng.shuffle(order)
    moved_any = False
    improved = True
    while improved:
        improved = False
        for node in order:
            current = label[node]
            node_degree = degree[node]
            links: dict = defaultdict(float)
            for other, weight in graph[node].items():
                if other != node:
                    links[label[other]] += weight
            label_degree[current] -= node_degree
            best = current
            best_gain = (
                links.get(current, 0.0) - label_degree[current] * node_degree / total
            )
            for candidate, weight in links.items():
                gain = weight - label_degree[candidate] * node_degree / total
                if gain > best_gain + 1e-12:
                    best, best_gain = candidate, gain
            label_degree[best] += node_degree
            if best != current:
                label[node] = best
                improved = True
                moved_any = True
    return label, moved_any


def purity(regions: dict, attribute: dict) -> float:
    by_region: dict = defaultdict(Counter)
    for node, region in regions.items():
        by_region[region][attribute[node]] += 1
    return sum(counts.most_common(1)[0][1] for counts in by_region.values()) / max(
        len(regions), 1
    )


def random_purities(regions: dict, attribute: dict, rng: random.Random) -> list[float]:
    nodes = list(regions)
    labels = [regions[node] for node in nodes]
    scores = []
    for _ in range(PERMUTATIONS):
        rng.shuffle(labels)
        scores.append(purity(dict(zip(nodes, labels)), attribute))
    return scores


def depths(regions: dict, directed: dict, start) -> dict:
    flow: dict = defaultdict(Counter)
    for (source, target), weight in directed.items():
        a, b = regions[source], regions[target]
        if a != b:
            flow[a][b] += weight
    out = {region: sum(targets.values()) for region, targets in flow.items()}
    distance = {start: 0.0}
    queue = [(0.0, start)]
    while queue:
        cost, region = heappop(queue)
        if cost > distance[region]:
            continue
        for target, weight in flow.get(region, {}).items():
            step = -math.log(weight / out[region])
            if cost + step < distance.get(target, math.inf):
                distance[target] = cost + step
                heappush(queue, (cost + step, target))
    return distance, flow


def holders(game: Game, entries: list[Entry], coarsen: str) -> set[int]:
    best: dict = {}
    for index, entry in enumerate(entries):
        slot = (entry.progress, entry.place, game.identity(entry, coarsen))
        for preference, value in enumerate(game.preferences(entry)):
            current = best.get((slot, preference))
            if current is None or value > current[0]:
                best[(slot, preference)] = (value, index)
    return {index for _, index in best.values()}


def report(game: Game, path: str) -> dict:
    rows = []
    with open(path) as source:
        for line in source:
            sequence, entry, parent, key, progress, place, identity = json.loads(line)
            rows.append(
                Entry(
                    sequence,
                    entry,
                    parent,
                    key,
                    frozen(progress),
                    frozen(place),
                    frozen(identity),
                )
            )
    rows.sort(key=lambda row: (row.sequence, row.id))
    first_tier: dict = {}
    for index, row in enumerate(rows):
        first_tier.setdefault(row.progress, index)
    checkpoints = sorted(index for index in first_tier.values() if index > 0)
    if game.name == "smb":
        checkpoints = [
            index
            for index in checkpoints
            if rows[index].progress[2] == 0 or rows[index].progress[:2] == (3, 3)
        ]
    id_index = {row.id: index for index, row in enumerate(rows)}
    directed: Counter = Counter()
    position = [(row.place, row.identity) for row in rows]
    start = position[0]
    results = []
    cursor = 0
    rng = random.Random(SEED)
    for checkpoint in checkpoints:
        while cursor < checkpoint:
            parent = id_index.get(rows[cursor].parent)
            if parent is not None and parent < cursor:
                directed[(position[parent], position[cursor])] += 1
            cursor += 1
        adjacency: dict = defaultdict(lambda: defaultdict(float))
        for (source, target), weight in directed.items():
            if source != target:
                adjacency[source][target] += weight
                adjacency[target][source] += weight
        nodes = sorted({position[index] for index in range(checkpoint)}, key=repr)
        regions = louvain(nodes, adjacency, random.Random(SEED))
        entry_of = {}
        for index in range(checkpoint):
            entry_of[position[index]] = rows[index]
        rooms = {node: game.room(entry_of[node]) for node in nodes}
        areas = {node: game.area(entry_of[node]) for node in nodes}
        room_purity = purity(regions, rooms)
        area_purity = purity(regions, areas)
        room_random = random_purities(regions, rooms, rng)
        area_random = random_purities(regions, areas, rng)
        distance, flow = depths(regions, directed, regions[start])
        deepest = max(distance, key=lambda region: (distance[region], repr(region)))
        arrival = rows[checkpoint]
        parent = id_index.get(arrival.parent)
        arrival_region = (
            regions.get(position[parent])
            if parent is not None and parent < checkpoint
            else None
        )
        neighbours = set(flow.get(deepest, {})) | {
            region for region, targets in flow.items() if deepest in targets
        }
        ranked = sorted(distance.values())
        arrival_rank = (
            sum(1 for value in ranked if value <= distance[arrival_region])
            / len(ranked)
            if arrival_region in distance
            else None
        )
        deepest_rooms = Counter(
            rooms[node] for node, region in regions.items() if region == deepest
        )
        entries = rows[:checkpoint]
        fine = holders(game, entries, "")
        slots = {(entry.progress, entry.place, entry.identity) for entry in entries}
        cells = {(entry.progress, entry.place) for entry in entries}
        coarsened = {}
        for coarsen in game.coarsenings():
            kept = holders(game, entries, coarsen)
            coarsened[coarsen] = {
                "displaced": len(fine - kept),
                "displaced_share": len(fine - kept) / max(len(fine), 1),
                "arrival_parent_kept": parent in kept if parent is not None else None,
            }
        results.append(
            {
                "checkpoint_entries": checkpoint,
                "sequence": arrival.sequence,
                "next_tier": repr(arrival.progress),
                "positions": len(nodes),
                "regions": len(set(regions.values())),
                "room_purity": room_purity,
                "room_random_mean": sum(room_random) / len(room_random),
                "room_random_max": max(room_random),
                "area_purity": area_purity,
                "area_random_mean": sum(area_random) / len(area_random),
                "area_random_max": max(area_random),
                "deepest_depth": distance[deepest],
                "deepest_rooms": [
                    [repr(room), count] for room, count in deepest_rooms.most_common(3)
                ],
                "arrival_in_deepest": arrival_region == deepest,
                "arrival_beside_deepest": arrival_region in neighbours,
                "arrival_depth_rank": arrival_rank,
                "slots_per_cell": len(slots) / max(len(cells), 1),
                "entries_per_slot": checkpoint / max(len(slots), 1),
                "holders": len(fine),
                "arrival_parent_held": parent in fine if parent is not None else None,
                "coarsened": coarsened,
            }
        )
        print(json.dumps(results[-1]), file=sys.stderr, flush=True)
    scored = [row for row in results if row["room_random_max"] < 1.0]
    wide = [
        row["room_purity"] > row["room_random_max"]
        and row["room_purity"] >= row["room_random_mean"] + 0.2
        for row in scored
    ]
    deep = [
        row["arrival_in_deepest"] or row["arrival_beside_deepest"] for row in results
    ]
    return {
        "format": "regions-v1",
        "game": game.name,
        "entries": len(rows),
        "checkpoints": results,
        "purity_wide": sum(wide),
        "deepest_hits": sum(deep),
        "count": len(results),
        "purity_scored": len(scored),
        "verdict_a": sum(wide) == len(scored),
        "verdict_b": sum(deep) * 2 > len(results),
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("game", choices=["metroid", "smb"])
    parser.add_argument("entries")
    args = parser.parse_args()
    print(json.dumps(report(Game(args.game), args.entries)))


if __name__ == "__main__":
    main()
