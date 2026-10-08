# SPDX-License-Identifier: AGPL-3.0-or-later
"""Force the same WAL race on affected and fixed SQLite, then inspect a branch."""
import argparse
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "faults" / "python"))
from harmony_test import Harmony, Schedule, Site

BEFORE_CHECKPOINT = Site("sqlite.wal.before_checkpoint")
LOSS = "no-lost-committed-writes"


def race(park=True):
    schedule = Schedule()
    if park:
        schedule.park("checkpoint", BEFORE_CHECKPOINT, for_="20s")
    return (schedule
            .hook("start-checkpoint", wait="35s")
            .hook("start-writer")
            .wait("40s"))


def test_wal_reset(harmony, affected_recipe, fixed_recipe):
    for recipe in (affected_recipe, fixed_recipe):
        harmony.prepare(recipe)
    affected = harmony.branch("affected", race(), recipe=affected_recipe, repeat=2)
    fixed = harmony.branch("fixed", race(), recipe=fixed_recipe, repeat=2)
    for branch in (affected, fixed):
        branch.parked(BEFORE_CHECKPOINT).observed(
            "writer-first-commit-completed", "writer-finished-during-pause",
            "final-canary-read-completed").identical()
    affected.violated(LOSS)
    fixed.clean()
    harmony.branch("control", race(park=False), recipe=affected_recipe).observed(
        "writer-first-commit-completed", "final-canary-read-completed").clean()
    if "committed=2 recovered=1" not in affected.logs(contains="recovered="):
        raise AssertionError("affected logs lack the lost write")
    if f"violation: {LOSS}" not in affected.timeline():
        raise AssertionError("affected timeline lacks the violation")
    earlier = harmony.branch("before-failure", source=affected, rewind=1, stop=True)
    neighborhood = harmony.search("neighborhood", source=earlier, executions=4)
    if neighborhood.executions < 1:
        raise AssertionError("search from the pre-failure branch executed nothing")
    print(f"Debug: harmony branch {earlier.path} --shell")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--affected", type=Path, default=Path(__file__).with_name("affected.toml"))
    parser.add_argument("--fixed", type=Path, default=Path(__file__).with_name("fixed.toml"))
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--harmony", default="harmony")
    args = parser.parse_args()
    test_wal_reset(Harmony(args.out, args.harmony), args.affected, args.fixed)
