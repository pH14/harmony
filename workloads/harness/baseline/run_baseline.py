#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Run random-schedule tests in parallel for a fixed wall budget.

Each test writes a fresh schedule with random_schedule.py and runs it with
`harmony debug run`, so boot, workload, check and fault execution match a
search of the same image. Results go to OUT/results.jsonl, one line per test.
"""

import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent


def report(path):
    try:
        return json.loads((path / "report.json").read_text())
    except (OSError, ValueError):
        return {}


def violations(rep):
    found = set()

    def walk(value):
        if isinstance(value, dict):
            for key, item in value.items():
                if key == "violations" and isinstance(item, list):
                    for entry in item:
                        found.add(entry if isinstance(entry, str) else json.dumps(entry, sort_keys=True)[:200])
                else:
                    walk(item)
        elif isinstance(value, list):
            for item in value:
                walk(item)

    walk(rep)
    return sorted(found)


def start(args, out, slot, seed):
    name = out / f"t{seed:06d}"
    actions = Path(f"{name}.actions.json")
    with actions.open("w") as handle:
        subprocess.run(
            [
                sys.executable,
                str(HERE / "random_schedule.py"),
                "--vocabulary", args.vocabulary,
                "--mode", args.mode,
                "--seed", str(seed),
                "--length", str(args.length),
                "--interval", str(args.interval),
                *(["--heal-hook", str(args.heal_hook)] if args.heal_hook is not None else []),
            ],
            stdout=handle,
            check=True,
        )
    env = dict(os.environ)
    env["HARMONY_CACHE_DIR"] = str(out / "cache" / f"slot{slot}")
    env["TMPDIR"] = str(out / "tmp" / f"slot{slot}")
    Path(env["HARMONY_CACHE_DIR"]).mkdir(parents=True, exist_ok=True)
    Path(env["TMPDIR"]).mkdir(parents=True, exist_ok=True)
    log = open(f"{name}.log", "w")
    proc = subprocess.Popen(
        [
            args.harmony, "debug", "run", args.image,
            "--config", args.config,
            "--seed", str(seed),
            "--actions", str(actions),
            "--out", str(name),
        ],
        stdout=log,
        stderr=subprocess.STDOUT,
        env=env,
        start_new_session=True,
    )
    return {"proc": proc, "seed": seed, "slot": slot, "name": name, "begin": time.time(), "log": log}


def finish(job, t0, code, cut):
    job["log"].close()
    rep = report(job["name"])
    found = bool(rep.get("bug_found"))
    record = {
        "seed": job["seed"],
        "slot": job["slot"],
        "start_s": round(job["begin"] - t0, 1),
        "end_s": round(time.time() - t0, 1),
        "exit": code,
        "cut_at_deadline": cut,
        "bug_found": found,
        "execution_ticks": rep.get("execution_ticks"),
        "violations": violations(rep),
    }
    if not found:
        shutil.rmtree(job["name"], ignore_errors=True)
        if not cut and code == 0:
            Path(f"{job['name']}.log").unlink(missing_ok=True)
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--harmony", required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--config", required=True, help="resolved.toml of a search of IMAGE")
    parser.add_argument("--vocabulary", required=True)
    parser.add_argument("--heal-hook", type=int)
    parser.add_argument("--mode", choices=("dense", "sparse"), required=True)
    parser.add_argument("--length", type=float, required=True)
    parser.add_argument("--interval", type=float, default=10.0)
    parser.add_argument("--first-seed", type=int, default=1)
    parser.add_argument("--slots", type=int, default=4)
    parser.add_argument("--minutes", type=float, default=60.0)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    (out / "params.json").write_text(json.dumps(vars(args), indent=1) + "\n")
    results = (out / "results.jsonl").open("a")
    t0 = time.time()
    deadline = t0 + args.minutes * 60
    seed = args.first_seed
    running = {}
    while True:
        now = time.time()
        if now < deadline:
            for slot in range(args.slots):
                if slot not in running:
                    running[slot] = start(args, out, slot, seed)
                    seed += 1
        for slot, job in list(running.items()):
            code = job["proc"].poll()
            if code is None and time.time() >= deadline:
                os.killpg(job["proc"].pid, signal.SIGKILL)
                code = job["proc"].wait()
                record = finish(job, t0, code, True)
            elif code is not None:
                record = finish(job, t0, code, False)
            else:
                continue
            del running[slot]
            results.write(json.dumps(record) + "\n")
            results.flush()
        if not running and time.time() >= deadline:
            break
        time.sleep(0.5)
    shutil.rmtree(out / "cache", ignore_errors=True)
    shutil.rmtree(out / "tmp", ignore_errors=True)


if __name__ == "__main__":
    main()
