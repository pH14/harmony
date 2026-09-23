"""Score Metroid ladder runs for `eval.py ladder`: segments passed in a row from
each chain root.

Each source is a matrix directory written by `eval.py run` over
`metroid-ladder.json`, or a JSON file `eval.py ladder --out` wrote. A root holds every
milestone its genesis state already satisfies at execution one, and every
milestone before the deepest one it satisfies, because the area milestones
name where Samus is and a root past an area has left it. The ladder for that
root is the chain's milestone sequence after the deepest milestone the root
holds. A milestone passes when at least two seeds reach it, and
the score is the number of passed milestones in a row from the front of the
ladder. Several sources print side by side, one column per build. A second
table gives each root's continuation graph counters per build: jobs,
landings, replacements and the longest wave, ranged over the root's seeds,
and a third gives the input actions and emulator work per execution, the
share of executions that replayed a splice donor's route, and the hours
each cell ran, so builds compare on emulation as well as executions.
A seed that reaches the ending has passed every milestone before it.
"""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from milestones import progress_of  # noqa: E402

LADDER = [
    "brinstar",
    "norfair",
    "ridley_defeated",
    "ice_beam",
    "tourian",
    "tourian_far",
    "tourian_bottom",
    "tourian_approach",
    "tourian_end",
    "zebetite_destroyed",
    "mother_brain_room",
    "mother_brain_defeated",
    "escape_started",
    "ending",
]

PASS_SEEDS = 2

GRAPH = ["jobs", "landed", "replaced", "longest_wave"]


def first_seen(summary, cell_dir):
    progress = progress_of(summary, cell_dir) or {}
    diagnostics = progress.get("workload_diagnostics") or {}
    named = (diagnostics.get("named_progress") or {}).get("first_seen") or {}
    return (
        {
            name: named[name]["execution"]
            for name in LADDER
            if named.get(name) and named[name].get("execution") is not None
        },
        progress.get("executions"),
        progress.get("continuations") or {},
        {
            "suffix_actions": (progress.get("coordinator") or {}).get("suffix_actions"),
            "suffix_cost": (progress.get("coordinator") or {}).get("suffix_cost"),
            "splice_jobs": (progress.get("coordinator") or {}).get("splice_jobs"),
            "elapsed_millis": progress.get("search_elapsed_millis"),
        },
    )


def collect(source):
    source = Path(source)
    if source.is_file():
        return json.loads(source.read_text())
    roots = {}
    build = None
    for cell in sorted(source.iterdir()):
        path = cell / "summary.json"
        if not path.is_file():
            continue
        summary = json.loads(path.read_text())
        request = summary.get("search_request") or {}
        if request.get("game") != "metroid":
            continue
        build = build or ((summary.get("build") or {}).get("binary_sha256") or "")[:12]
        seen, executions, continuations, cost = first_seen(summary, cell)
        root = roots.setdefault(
            summary.get("case"),
            {"root_input": request.get("root_input"), "seeds": {}},
        )
        root["seeds"][str(request.get("seed"))] = {
            "status": summary.get("status"),
            "exit_code": summary.get("exit_code"),
            "executions": executions,
            "first_execution": seen,
            "continuations": {name: continuations.get(name) for name in GRAPH},
            "cost": cost,
        }
    return {
        "format": "harmony-metroid-ladder-v1",
        "matrix": source.name,
        "build": build,
        "ladder": LADDER,
        "roots": roots,
    }


def score(root):
    seeds = list(root["seeds"].values())
    held_at_root = [
        index
        for index, name in enumerate(LADDER)
        if sum(1 for seed in seeds if seed["first_execution"].get(name) == 1)
        >= PASS_SEEDS
    ]
    front = max(held_at_root, default=-1) + 1
    held = set(LADDER[:front])
    ladder = LADDER[front:]
    passed = []
    for name in ladder:
        reached = sorted(
            execution
            for execution in (
                reached_at(seed["first_execution"], name) for seed in seeds
            )
            if execution is not None
        )
        if len(reached) < PASS_SEEDS:
            break
        passed.append((name, reached))
    return held, ladder, passed


def reached_at(first_execution, name):
    own = first_execution.get(name, 0)
    if own > 1:
        return own
    ending = first_execution.get("ending", 0)
    if ending > 1:
        return ending
    return None


def case_order(case):
    return int(case.split("-seg")[-1])


def render(documents):
    cases = sorted(
        {case for document in documents for case in document["roots"]}, key=case_order
    )
    builds = [document.get("build") or document.get("matrix") for document in documents]
    header = ["root", "next"] + [f"{build} passed" for build in builds]
    lines = [header]
    for case in cases:
        line = [case]
        first_next = None
        for document in documents:
            root = document["roots"].get(case)
            if root is None:
                line.append("-")
                continue
            held, ladder, passed = score(root)
            first_next = first_next or (ladder[0] if ladder else "-")
            last = passed[-1] if passed else None
            detail = f"{len(passed)}"
            if last:
                detail += (
                    f" to {last[0]} at {', '.join(f'{value:,}' for value in last[1])}"
                )
            failures = [
                seed
                for seed, cell in root["seeds"].items()
                if cell.get("exit_code") not in (0, None)
            ]
            if failures:
                detail += f" (exit on seeds {', '.join(failures)})"
            line.append(detail)
        line.insert(1, first_next or "-")
        lines.append(line)
    print_table(lines)
    print()
    graph = [["root"] + [f"{build} graph" for build in builds]]
    for case in cases:
        line = [case]
        for document in documents:
            root = document["roots"].get(case)
            line.append(graph_detail(root) if root else "-")
        graph.append(line)
    print_table(graph)
    print()
    cost = [["root"] + [f"{build} per execution" for build in builds]]
    for case in cases:
        line = [case]
        for document in documents:
            root = document["roots"].get(case)
            line.append(cost_detail(root) if root else "-")
        cost.append(line)
    print_table(cost)


def cost_detail(root):
    actions = work = executions = splices = 0
    hours = []
    for seed in root["seeds"].values():
        cost = seed.get("cost") or {}
        if cost.get("suffix_actions") is None or not seed.get("executions"):
            return "-"
        actions += cost["suffix_actions"]
        work += cost["suffix_cost"]
        executions += seed["executions"]
        splices += cost.get("splice_jobs") or 0
        hours.append((cost.get("elapsed_millis") or 0) / 3_600_000)
    return (
        f"actions {actions / executions:.1f}, work {work / executions:.0f}, "
        f"splices {splices / executions:.0%}, hours {min(hours):.1f}-{max(hours):.1f}"
    )


def compact(value):
    if value >= 1_000_000:
        return f"{value / 1_000_000:.1f}M"
    if value >= 1_000:
        return f"{value / 1_000:.0f}K"
    return str(value)


def graph_detail(root):
    parts = []
    for name in GRAPH:
        values = sorted(
            seed.get("continuations", {}).get(name)
            for seed in root["seeds"].values()
            if seed.get("continuations", {}).get(name) is not None
        )
        if not values:
            return "-"
        low, high = compact(values[0]), compact(values[-1])
        span = low if low == high else f"{low}-{high}"
        parts.append(f"{name.replace('longest_', '')} {span}")
    return ", ".join(parts)


def print_table(lines):
    widths = [max(len(line[index]) for line in lines) for index in range(len(lines[0]))]
    for line in lines:
        print("  ".join(part.ljust(width) for part, width in zip(line, widths)))


def score_sources(sources, out=None):
    documents = [collect(source) for source in sources]
    render(documents)
    if out:
        out.mkdir(parents=True, exist_ok=True)
        for document in documents:
            path = out / f"{document['matrix']}.json"
            path.write_text(json.dumps(document, indent=1, sort_keys=True) + "\n")
            print(f"wrote {path}", file=sys.stderr)
