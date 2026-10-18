# SPDX-License-Identifier: AGPL-3.0-or-later
"""Run actual single-level Dissonance searches across the complete source index."""

import argparse
import asyncio
import json
import os
from pathlib import Path


async def run(args):
    index = json.loads(args.index.read_text())
    args.output.mkdir(parents=True, exist_ok=True)
    queue = asyncio.Queue()
    ledger = {"schema": 1, "round": args.round, "engine": "dissonance-wasm-single-level",
              "goal": "all indexed levels searched; candidate review is a separate required phase",
              "levels": {}}
    skip = set(args.skip.split(",")) if args.skip else set()
    for layout in index["levels"]:
        key = f"{layout['side']}:{layout['level']}"
        directory = args.output / f"{layout['side']}-{layout['level']}"
        ledger["levels"][key] = {"name": layout["name"], "directory": str(directory), "status": "queued"}
        if key not in skip:
            queue.put_nowait((key, layout, directory))

    def save():
        for key, entry in ledger["levels"].items():
            report_path = Path(entry["directory"]) / "report.json"
            if report_path.exists():
                try:
                    report = json.loads(report_path.read_text())
                    for field in ["status", "max_x", "cleared", "coverage_cells", "dual_inspected", "modes"]:
                        entry[field] = report.get(field)
                    if entry.get("exit_code"):
                        entry["status"] = "failed"
                    entry["executions"] = sum(root.get("executions", 0) for root in report["roots"])
                    entry["candidates"] = len(report["candidates"])
                except (OSError, ValueError, KeyError):
                    pass
        ledger["searched_levels"] = sum(e["status"] == "searched" for e in ledger["levels"].values())
        ledger["executions"] = sum(e.get("executions", 0) for e in ledger["levels"].values())
        ledger["candidates"] = sum(e.get("candidates", 0) for e in ledger["levels"].values())
        path = args.output / "ledger.json"
        temporary = path.with_suffix(".tmp")
        temporary.write_text(json.dumps(ledger, indent=2) + "\n")
        os.replace(temporary, path)

    async def worker():
        while not queue.empty():
            key, layout, directory = queue.get_nowait()
            report_path = directory / "report.json"
            if report_path.exists() and json.loads(report_path.read_text()).get("status") == "searched":
                queue.task_done()
                continue
            ledger["levels"][key]["status"] = "starting"
            log_path = args.output / f"{layout['side']}-{layout['level']}.log"
            command = ["node", str(Path(__file__).with_name("soak-level.mjs")), str(args.index), str(args.roms),
                       str(args.runtime), str(args.bindings), str(args.wasm), str(directory),
                       layout["side"], str(layout["level"]), str(args.round)]
            with log_path.open("wb") as log:
                process = await asyncio.create_subprocess_exec(*command, stdout=log, stderr=asyncio.subprocess.STDOUT)
                code = await process.wait()
            if code:
                ledger["levels"][key]["status"] = "failed"
                ledger["levels"][key]["exit_code"] = code
            save()
            print(key, ledger["levels"][key]["status"], ledger["levels"][key].get("executions", 0), flush=True)
            queue.task_done()

    async def heartbeat():
        while True:
            save()
            await asyncio.sleep(5)

    monitor = asyncio.create_task(heartbeat())
    await asyncio.gather(*(worker() for _ in range(args.workers)))
    monitor.cancel()
    save()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["index", "roms", "runtime", "bindings", "wasm", "output"]:
        parser.add_argument(name, type=lambda value: Path(value).resolve())
    parser.add_argument("--round", type=int, default=1)
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument("--skip", default="", help="externally running side:level keys, separated by commas")
    options = parser.parse_args()
    if options.round < 1 or options.workers < 1:
        parser.error("round and workers must be positive")
    asyncio.run(run(options))
