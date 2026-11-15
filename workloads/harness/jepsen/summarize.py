#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Summarize Jepsen NATS queue tests from a Jepsen store directory.

Prints one JSON object per test run with the verdict, the lost-write counts,
the test times of the first and last lost acknowledged publish and of the
last acknowledged publish, the first fault and its targets, the
whole-cluster outages, then the rows of runs.tsv with each test's wall time
from start to verdict.
"""

import json
import re
import sys
from pathlib import Path

OP = re.compile(r"^\{:index (\d+), :time (\d+), :type :(\w+), :process ([^,]+), :f :([\w-]+)(?:, :value (.*?))?"
                r"(?:, :error (.*))?\}$")
NODE_STATE = re.compile(r'"(n\d+)" :([\w-]+)')
NODES = 3
STRING = re.compile(r'"((?:[^"\\]|\\.)*)"')


def verdict(log):
    text = log.read_text(errors="replace") if log.exists() else ""
    if "Everything looks good!" in text:
        return "valid"
    if "Analysis invalid!" in text:
        return "invalid"
    if "Errors occurred during analysis" in text:
        return "unknown"
    if "Test crashed!" in text:
        return "crashed"
    return "incomplete"


def analyze(run):
    acked, read, read_by_node = {}, set(), {}
    down, outages, all_down, first_fault = set(), [], False, None
    for line in (run / "history.edn").read_text(errors="replace").splitlines():
        m = OP.match(line)
        if not m:
            continue
        _, time, kind, process, f, value, _ = m.groups()
        time = int(time) / 1e9
        if process == ":nemesis":
            if first_fault is None and kind == "info" and value and not value.startswith("{"):
                first_fault = f"{f} {value.lstrip(':')}"
            for node, state in NODE_STATE.findall(value or ""):
                if state == "killed":
                    down.add(node)
                elif state in ("started", "already-running"):
                    down.discard(node)
            if len(down) == NODES and not all_down:
                outages.append(time)
            all_down = len(down) == NODES
            continue
        if f == "publish" and kind == "ok":
            acked[STRING.findall(value)[0]] = time
        elif f in ("fetch", "next-message") and kind == "ok" and value:
            values = STRING.findall(value)
            read.update(values)
            if f == "fetch":
                read_by_node.setdefault(int(process) % NODES, set()).update(values)
    lost = {v: t for v, t in acked.items() if v not in read}
    result = {"acknowledged": len(acked), "read": len(read), "lost": len(lost),
              "whole_cluster_outages": len(outages),
              "lost_on_one_node": max((sum(1 for v in acked if v not in read_by_node.get(n, ())) for n in range(NODES)),
                                      default=0),
              "first_fault": first_fault}
    if outages:
        result["first_outage_s"] = round(outages[0], 1)
    if lost:
        first_lost = min(lost.values())
        result["first_lost_ack_s"] = round(first_lost, 1)
        result["last_lost_ack_s"] = round(max(lost.values()), 1)
        result["outages_before_last_lost_ack"] = sum(1 for t in outages if t <= max(lost.values()))
        later = [t for t in outages if t > max(lost.values())]
        if later:
            result["first_outage_after_last_lost_ack_s"] = round(later[0], 1)
    if acked:
        result["last_ack_s"] = round(max(acked.values()), 1)
    return result


def main():
    store = Path(sys.argv[1])
    runs = {}
    tsv = store / "runs.tsv"
    if tsv.exists():
        for row in tsv.read_text().splitlines()[1:]:
            project, version, start, end, status, options = row.split("\t", 5)
            runs[int(start)] = {"version": version, "wall_s": int(end) - int(start), "exit": int(status),
                                "options": options}
    for log in sorted(store.glob("*/2*/jepsen.log")):
        run = log.parent
        record = {"test": run.parent.name, "run": run.name, "verdict": verdict(log)}
        if (run / "history.edn").exists():
            record.update(analyze(run))
        print(json.dumps(record, sort_keys=True))
    if runs:
        print(json.dumps({"runs_tsv": list(runs.values())}))


if __name__ == "__main__":
    main()
