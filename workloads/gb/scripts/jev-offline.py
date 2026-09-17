#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["httpx>=0.27"]
# ///
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Ask Jev to rank live Pokemon Blue archive entries and score the answers
against the scripted route to Brock."""

import argparse
import json
import os
import statistics
import sys
import time
from pathlib import Path

import httpx

ENDPOINT = "https://api.typesafe.ai/v1/systemone"
MODEL = "jev-latest"
INPUT_DOLLARS_PER_MILLION = 0.042

# Mirrors MILESTONE_NAMES in workloads/gb/src/progress.rs; bits 0-6 are also the
# route flags in the archive key.
MILESTONE_NAMES = [
    "got_starter",
    "got_parcel",
    "delivered_parcel",
    "got_pokedex",
    "entered_viridian_forest",
    "entered_pewter",
    "entered_pewter_gym",
    "boulder_badge",
]

# Mirrors ActionKind::frame_budget in workloads/gb/src/target.rs. The searcher
# orders equal progress by accumulated frames, so the baseline must too.
ACTION_FRAMES = {
    "WalkTo": 1_650,
    "Interact": 750,
    "BattleMove": 1_350,
    "UseItem": 1_050,
    "Switch": 1_050,
    "Advance": 1_050,
}

GOAL = (
    "A search is playing Pokemon Blue from a new game. The goal is Brock's "
    "Boulder Badge in the Pewter City gym. The route is: take a starter from "
    "Oak's lab, win the rival battle, cross Route 1 to Viridian City, fetch "
    "Oak's parcel from the Viridian Poke Mart, carry it back to Pallet Town, "
    "take the Pokedex, go north through Viridian City to Route 2, cross "
    "Viridian Forest, reach Pewter City, and beat Brock. Each item below is a "
    "saved position the search can resume from."
)


def flag_names(flags):
    return [name for bit, name in enumerate(MILESTONE_NAMES) if flags & (1 << bit)]


def render(entry):
    key = entry["key"]
    return {
        "id": entry["id"],
        "map": key["map"],
        "cell": [key["cell_x"], key["cell_y"]],
        "badges": key["badges"],
        "reached": flag_names(key["route"]),
        "game_events_set": key["events"],
        "party_hp": key["party_hp"],
        "party_levels": key["party_levels"],
        "actions_to_reach": entry["actions_to_reach"],
        "frames_to_reach": entry["frames_to_reach"],
    }


def suffix_cost(actions):
    return sum(ACTION_FRAMES[action["kind"]] for action in actions)


def load_entries(reports):
    """Entries are stored as a suffix plus a parent, so the prefix is walked.

    Entry ids restart in every campaign, so the seed goes in the id the
    questions are keyed by."""
    entries = []
    for path in reports:
        report = json.loads(Path(path).read_text())
        archive = report.get("archive", report)
        seed = archive["seed"]
        reached = {}
        for entry in archive["entries"]:
            if "input" in entry:
                actions = entry["input"]["actions"]
                walked = (len(actions), suffix_cost(actions))
            else:
                parent = reached.get(entry["parent_id"])
                if parent is None:
                    continue
                suffix = entry["input_suffix"]
                walked = (
                    parent[0] + len(suffix),
                    parent[1] + suffix_cost(suffix),
                )
            reached[entry["id"]] = walked
            entries.append(
                {
                    **entry,
                    "seed": seed,
                    "id": f"{seed}:{entry['id']}",
                    "actions_to_reach": walked[0],
                    "frames_to_reach": walked[1],
                }
            )
    return entries


def route_positions(trace_path):
    """Map each place on the scripted route to how far along it is.

    The route crosses the same cells twice, so the badges and route flags are
    part of the place; without them a late revisit reads as early progress."""
    trace = json.loads(Path(trace_path).read_text())
    positions = {}
    total = len(trace["points"])
    for point in trace["points"]:
        place = place_of(point)
        positions.setdefault(place, point["action"] / max(total - 1, 1))
    return positions


def place_of(entry):
    key = entry["key"]
    return (
        key["badges"],
        key["route"],
        key["map"],
        key["cell_x"],
        key["cell_y"],
    )


def questions_for(batch):
    ids = [str(entry["id"]) for entry in batch]
    questions = {
        "choice": {
            "type": "choice",
            "instructions": (
                f"{GOAL} Which one of these saved positions leads soonest to "
                "the next milestone on the route to Brock?"
            ),
            "criteria": {entry_id: None for entry_id in ids},
        }
    }
    for entry_id in ids:
        questions[f"score:{entry_id}"] = {
            "type": "score",
            "instructions": (
                f"{GOAL} How far along the route to Brock is the position with "
                f"id {entry_id}?"
            ),
            "criteria": [
                "has just left the bedroom and holds no starter",
                "holds a starter and is on Route 1 or in Viridian City",
                "is carrying or has delivered Oak's parcel",
                "holds the Pokedex and is heading north through Route 2",
                "is inside Viridian Forest",
                "has reached Pewter City or the gym",
                "holds the Boulder Badge",
            ],
        }
        questions[f"noul:{entry_id}"] = {
            "type": "noul",
            "instructions": (
                f"{GOAL} Is the position with id {entry_id} a dead end for "
                "reaching Brock, meaning the search should spend no more draws "
                "on it?"
            ),
            "criteria": {
                "true": "further search from here cannot reach Brock any sooner",
                "false": "further search from here can still make progress",
            },
        }
    return questions


def ask(client, key, batch, retries):
    body = {
        "state": [render(entry) for entry in batch],
        "model": MODEL,
        "questions": questions_for(batch),
    }
    delay = 1.0
    for attempt in range(retries + 1):
        started = time.monotonic()
        response = client.post(
            ENDPOINT,
            headers={"Authorization": f"Bearer {key}"},
            json=body,
            timeout=120,
        )
        latency = time.monotonic() - started
        if response.status_code in (429, 529) and attempt < retries:
            time.sleep(delay)
            delay *= 2
            continue
        response.raise_for_status()
        return response.json(), latency
    raise RuntimeError("Jev kept returning a retryable status")


def searcher_rank(entry):
    """The archive's own preference: progress first, then cheaper in frames."""
    key = entry["key"]
    return (
        -bin(key["badges"]).count("1"),
        -bin(key["route"]).count("1"),
        -key["events"],
        entry["frames_to_reach"],
    )


def summarise(entries, answers, positions):
    by_id = {str(entry["id"]): entry for entry in entries}
    picks = []
    scores = {}
    dead_ends = {}
    for name, answer in answers.items():
        kind, _, entry_id = name.partition(":")
        if kind == "choice":
            picks = sorted(
                answer["probabilities"].items(), key=lambda item: -item[1]
            )
        elif kind == "score":
            scores[entry_id] = answer["score"]
        elif kind == "noul":
            dead_ends[entry_id] = answer["noul"]
    on_route = {
        entry_id: place_of(entry) in positions for entry_id, entry in by_id.items()
    }
    depth = {
        entry_id: positions.get(place_of(entry))
        for entry_id, entry in by_id.items()
    }
    ranked = sorted(by_id.values(), key=searcher_rank)
    baseline = [str(entry["id"]) for entry in ranked]
    jev = [entry_id for entry_id, _ in picks] or sorted(
        scores, key=lambda entry_id: -scores[entry_id]
    )
    return {
        "entries": len(by_id),
        "on_route_fraction": mean_or_none(
            [1.0 if value else 0.0 for value in on_route.values()]
        ),
        "jev_top5_on_route": mean_or_none(
            [1.0 if on_route[entry_id] else 0.0 for entry_id in jev[:5]]
        ),
        "searcher_top5_on_route": mean_or_none(
            [1.0 if on_route[entry_id] else 0.0 for entry_id in baseline[:5]]
        ),
        "jev_top5_depth": mean_or_none(
            [depth[entry_id] for entry_id in jev[:5] if depth[entry_id] is not None]
        ),
        "searcher_top5_depth": mean_or_none(
            [
                depth[entry_id]
                for entry_id in baseline[:5]
                if depth[entry_id] is not None
            ]
        ),
        "dead_end_on_route": mean_or_none(
            [dead_ends[entry_id] for entry_id in dead_ends if on_route[entry_id]]
        ),
        "dead_end_off_route": mean_or_none(
            [dead_ends[entry_id] for entry_id in dead_ends if not on_route[entry_id]]
        ),
        "score_depth_agreement": agreement(scores, depth),
        "searcher_depth_agreement": agreement(
            {entry_id: -rank for rank, entry_id in enumerate(baseline)}, depth
        ),
        "jev_order": jev[:10],
        "searcher_order": baseline[:10],
    }


def mean_or_none(values):
    return statistics.fmean(values) if values else None


def agreement(scores, depth):
    """Fraction of comparable pairs Jev's score orders the same way as the route."""
    pairs = [
        (entry_id, value)
        for entry_id, value in scores.items()
        if depth.get(entry_id) is not None
    ]
    agree = total = 0
    for index, (left, left_score) in enumerate(pairs):
        for right, right_score in pairs[index + 1 :]:
            if depth[left] == depth[right] or left_score == right_score:
                continue
            total += 1
            if (left_score > right_score) == (depth[left] > depth[right]):
                agree += 1
    return agree / total if total else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("reports", nargs="+", type=Path)
    parser.add_argument("--trace", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--batch", type=int, default=64)
    parser.add_argument("--batches", type=int, default=0)
    parser.add_argument("--retries", type=int, default=4)
    args = parser.parse_args()

    key = os.environ.get("TYPESAFE_API_KEY")
    if not key:
        print("TYPESAFE_API_KEY is not set", file=sys.stderr)
        return 2
    if not 1 <= args.batch <= 255:
        print("--batch must be between 1 and 255", file=sys.stderr)
        return 2

    entries = load_entries(args.reports)
    positions = route_positions(args.trace)
    batches = [
        entries[start : start + args.batch]
        for start in range(0, len(entries), args.batch)
    ]
    if args.batches:
        batches = batches[: args.batches]

    results = []
    input_tokens = 0
    latencies = []
    with httpx.Client() as client:
        for index, batch in enumerate(batches):
            answer, latency = ask(client, key, batch, args.retries)
            latencies.append(latency)
            input_tokens += answer.get("usage", {}).get("input_tokens", 0)
            results.append(summarise(batch, answer["answers"], positions))
            print(
                f"batch {index + 1}/{len(batches)} entries={len(batch)} "
                f"latency={latency:.2f}s",
                flush=True,
            )

    summary = {
        "format": "blue-jev-offline-v1",
        "reports": [str(path) for path in args.reports],
        "entries": len(entries),
        "batches": len(results),
        "input_tokens": input_tokens,
        "input_dollars": input_tokens / 1e6 * INPUT_DOLLARS_PER_MILLION,
        "latency_seconds": {
            "median": statistics.median(latencies) if latencies else None,
            "max": max(latencies) if latencies else None,
        },
        "jev_top5_on_route": mean_or_none(
            [item["jev_top5_on_route"] for item in results if item["jev_top5_on_route"] is not None]
        ),
        "searcher_top5_on_route": mean_or_none(
            [
                item["searcher_top5_on_route"]
                for item in results
                if item["searcher_top5_on_route"] is not None
            ]
        ),
        "jev_top5_depth": mean_or_none(
            [item["jev_top5_depth"] for item in results if item["jev_top5_depth"] is not None]
        ),
        "searcher_top5_depth": mean_or_none(
            [
                item["searcher_top5_depth"]
                for item in results
                if item["searcher_top5_depth"] is not None
            ]
        ),
        "dead_end_on_route": mean_or_none(
            [item["dead_end_on_route"] for item in results if item["dead_end_on_route"] is not None]
        ),
        "dead_end_off_route": mean_or_none(
            [
                item["dead_end_off_route"]
                for item in results
                if item["dead_end_off_route"] is not None
            ]
        ),
        "score_depth_agreement": mean_or_none(
            [
                item["score_depth_agreement"]
                for item in results
                if item["score_depth_agreement"] is not None
            ]
        ),
        "searcher_depth_agreement": mean_or_none(
            [
                item["searcher_depth_agreement"]
                for item in results
                if item["searcher_depth_agreement"] is not None
            ]
        ),
        "batch_results": results,
    }
    args.out.write_text(json.dumps(summary, indent=1))
    print(json.dumps({k: v for k, v in summary.items() if k != "batch_results"}, indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
