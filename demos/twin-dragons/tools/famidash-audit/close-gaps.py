# SPDX-License-Identifier: AGPL-3.0-or-later
"""Continue Dissonance from ordinary input prefixes before uncovered frontiers."""

import argparse
import asyncio
import json
import os
from pathlib import Path


async def run(args):
    args.output.mkdir(parents=True, exist_ok=True)
    queue = asyncio.Queue()
    ledger = json.loads((args.previous / "ledger.json").read_text())
    for key, level in ledger["levels"].items():
        if level.get("cleared"):
            continue
        side, number = key.split(":")
        name = f"{side}-{number}"
        existing = args.output / name / "report.json"
        if existing.exists() and json.loads(existing.read_text()).get("status") == "searched":
            continue
        frontier = args.previous / name / "frontier.json"
        if not frontier.exists():
            raise RuntimeError(f"missing reachable frontier: {frontier}")
        plan = args.output / f"{name}-plan.json"
        frames = sum(action["frames"] for action in json.loads(frontier.read_text())["actions"])
        plan.write_text(json.dumps([{"input": str(frontier), "trim": trim, "budget": args.budget}
                                    for trim in sorted({min(value, frames) for value in (128, 384, 768)})]))
        queue.put_nowait((side, number, name, plan))

    async def worker():
        while not queue.empty():
            side, number, name, plan = queue.get_nowait()
            command = ["node", str(Path(__file__).with_name("soak-level.mjs")), str(args.index),
                       str(args.roms), str(args.runtime), str(args.bindings), str(args.wasm),
                       str(args.output / name), side, number, str(args.round)]
            environment = dict(os.environ, FAMIDASH_ROOT_PLAN=str(plan))
            with (args.output / f"{name}.log").open("wb") as log:
                process = await asyncio.create_subprocess_exec(*command, env=environment,
                                                              stdout=log, stderr=asyncio.subprocess.STDOUT)
                code = await process.wait()
            print(name, "finished" if not code else f"failed:{code}", flush=True)
            queue.task_done()

    await asyncio.gather(*(worker() for _ in range(args.workers)))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["index", "roms", "runtime", "bindings", "wasm", "previous", "output"]:
        parser.add_argument(name, type=lambda value: Path(value).resolve())
    parser.add_argument("--round", type=int, default=2)
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument("--budget", type=int, default=50000)
    args = parser.parse_args()
    if min(args.round, args.workers, args.budget) < 1:
        parser.error("round, workers and budget must be positive")
    asyncio.run(run(args))
