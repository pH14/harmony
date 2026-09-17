#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Merge exported case rosters without treating missing jobs as passing evidence."""

import argparse
import html
import importlib.util
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("nes_evaluation", ROOT / "benchmarks/search/eval.py")
EVAL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EVAL)


def collect(suite, cases):
    expected = {job["id"]: job for job in EVAL.expand_suite(suite)}
    found = {}
    issues = []
    for roster in sorted(cases.glob("*/roster.json")):
        try:
            if json.loads(roster.with_name("suite.json").read_text()) != suite:
                raise ValueError("artifact manifest differs from the registered suite")
            rows = json.loads(roster.read_text())
            if not isinstance(rows, list):
                raise ValueError("roster is not an array")
            for row in rows:
                cell = row["cell"]
                if cell not in expected or cell in found:
                    raise ValueError(f"unknown or duplicate cell: {cell}")
                if row.get("case") != expected[cell]["case"]["id"]:
                    raise ValueError(f"case identity differs for {cell}")
                media = row.get("media") or {"available": False, "reason": "the roster predates media"}
                if not media.get("available"):
                    issues.append(f"{cell}: media unavailable: {media.get('reason', 'unrecorded')}")
                found[cell] = {**row, "report": f"cases/{roster.parent.name}/index.html"}
        except (OSError, ValueError, KeyError, TypeError) as error:
            issues.append(f"{roster.parent.name}: {error}")
    rows = []
    for cell, job in expected.items():
        if cell in found:
            rows.append(found[cell])
        else:
            issues.append(f"missing evidence: {cell}")
            rows.append({"cell": cell, "case": job["case"]["id"], "game": job["case"]["game"],
                         "seed": job["request"]["seed"], "status": "missing", "outcome": "missing evidence"})
    return rows, issues


def render(rows, issues):
    table = []
    for row in rows:
        cell = html.escape(row["cell"])
        if row.get("report"):
            cell = f'<a href="{html.escape(row["report"], quote=True)}">{cell}</a>'
        media = row.get("media") or {}
        if media.get("available"):
            link = f'cases/{row["report"].split("/")[1]}/{media["mp4"]}' if row.get("report") else media["mp4"]
            film = f'<a href="{html.escape(link, quote=True)}">film with game audio</a>'
        else:
            film = html.escape(str(media.get("reason", "unavailable")))
        table.append(f'<tr><td>{cell}</td><td>{html.escape(str(row.get("status", "unavailable")))}</td>'
                     f'<td>{html.escape(str(row.get("outcome", "unavailable")))}</td>'
                     f'<td>{film}</td></tr>')
    return ('<!doctype html><meta charset="utf-8"><title>NES nightly roster</title>'
            '<h1>NES nightly roster</h1><p>Independent case jobs; missing evidence remains visible. '
            'Each case report retains budgets, progress, replay confirmation, media and resource costs. '
            'Every film replays the input its own run recorded and carries game audio.</p>'
            '<p><a href="roster.json">Combined roster JSON</a></p><ul>'
            + ''.join(f'<li>{html.escape(issue)}</li>' for issue in issues)
            + '</ul><table><tr><th>Cell / report</th><th>Status</th><th>Outcome</th><th>Media</th></tr>'
            + ''.join(table) + '</table>')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cases", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    suite = json.loads((ROOT / "benchmarks/search/nightly.json").read_text())
    rows, issues = collect(suite, args.cases)
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "roster.json").write_text(json.dumps(rows, indent=2) + "\n")
    (args.out / "issues.json").write_text(json.dumps(issues, indent=2) + "\n")
    (args.out / "index.html").write_text(render(rows, issues))
    for issue in issues:
        print(issue)
    return int(bool(issues))


if __name__ == "__main__":
    raise SystemExit(main())
