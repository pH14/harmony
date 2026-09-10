# SPDX-License-Identifier: AGPL-3.0-or-later
"""Build the investigation panel's workspace fixture.

The panel measures whether an agent can drive the shipping CLI over a recorded
finding. It needs a workspace, and a workspace normally comes from a search on
a KVM host. Two sources are supported:

* `recorded` — a directory a real `harmony search` wrote. Use this wherever KVM
  execution is available.
* `derived` — a workspace assembled here from the historical case's own bundle,
  witness action list, and detector output format, with no VM. The retained
  evidence is written from that ground truth rather than captured from a run.

Every fixture records which source it came from in its own facts, and the
runner copies that word into each attempt's record. A derived fixture measures
CLI usability. It does not measure a real investigation, and a report must not
claim it did.
"""

from __future__ import annotations

import hashlib
import json
import shutil
from pathlib import Path

FORMAT = "harmony-workspace-v1"

CASE = Path("bugs/historical/postgres-cic-corruption")

# The bundle the workload image carries, with the declaration lines that let
# `inspect` name what a property means and who evaluates it. Everything here
# except the declarations is the case's own bundle.
BUNDLE = """\
setup /opt/harmony/setup.sh
node postgres /opt/harmony/node.sh
ready /opt/harmony/ready.sh
hook 1 /opt/harmony/hooks.sh 1
hook 2 /opt/harmony/hooks.sh 2
hook 3 /opt/harmony/hooks.sh 3
hook 4 /opt/harmony/hooks.sh 4
describe node 0 the seeded cluster, running as uid 70
describe hook 1 HOT-update churn through the seeded churn procedure
describe hook 2 DROP INDEX then CREATE INDEX CONCURRENTLY on cic(k)
describe hook 3 pg_amcheck --heapallindexed against cic_k_idx
describe hook 4 VACUUM cic, the only pruning event outside the churn
assert always 2 from 3 every live heap tuple in cic has a matching entry in cic_k_idx
assert sometimes 21 from 1 the churn procedure completed a round of HOT updates
assert sometimes 23 from 2 a concurrent index build completed
assert sometimes 25 from 4 a manual vacuum pruned the table
assert reachable 20 from 1 the churn hook started
assert reachable 22 from 2 the build hook started
assert reachable 24 from 3 pg_amcheck reached a verdict about the index
assert reachable 26 from 3 the checker found the server down and stayed silent
assert reachable 27 from 3 the checker failed for a reason other than the index
diagnostic amcheck-output sh -c cat /run/amcheck.*.out
diagnostic hook-errors sh -c cat /run/hook.err
diagnostic index-list /usr/lib/postgresql/bin/psql -h /tmp -U postgres -d faultlab -c \\di
"""

# pg_amcheck's own wording for this corruption, which the case's hook greps for.
AMCHECK = """\
heap tuple (1712,14) from table "cic" lacks matching index tuple within index "cic_k_idx"
heap tuple (1712,21) from table "cic" lacks matching index tuple within index "cic_k_idx"
heap tuple (2044,3) from table "cic" lacks matching index tuple within index "cic_k_idx"
"""

CONSOLE = """\
FAULT_INIT_STAGE=chroot-agent
harmony-fault-agent: bundle accepted: 1 node, 4 hooks
harmony-fault-agent: setup complete
harmony-fault-agent: node 0 started
harmony-fault-agent: hook 1 started
harmony-fault-agent: hook 2 started
harmony-fault-agent: hook 4 started
harmony-fault-agent: hook 3 started
harmony-fault-agent: hook 3 reported @always 2 0
"""

EVENTS = """\
position=141 virtual_time=11800000000 event=assert_reachable point=22
position=142 virtual_time=11940000000 event=assert_sometimes point=23
position=147 virtual_time=12080000000 event=assert_reachable point=25
position=151 virtual_time=12240000000 event=assert_reachable point=24
position=152 virtual_time=12300000000 event=assert_always point=2 cond=0
"""


def _hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _declarations() -> dict:
    """Parse BUNDLE the way `faults_workload::declarations` does."""
    nodes, hooks, assertions, diagnostics = [], [], [], []
    node_text: dict[str, str] = {}
    hook_text: dict[int, str] = {}
    setup: list[str] = []
    ready: list[str] = []
    for line in BUNDLE.splitlines():
        words = line.split()
        if not words or words[0].startswith("#"):
            continue
        keyword, rest = words[0], words[1:]
        if keyword == "node":
            nodes.append({"id": len(nodes), "name": rest[0], "argv": rest[1:],
                          "description": None})
        elif keyword == "hook":
            hooks.append({"id": int(rest[0]), "argv": rest[1:], "description": None})
        elif keyword == "setup":
            setup = rest
        elif keyword == "ready":
            ready = rest
        elif keyword == "describe":
            what, subject, text = rest[0], rest[1], " ".join(rest[2:])
            if what == "node":
                node_text[subject] = text
            else:
                hook_text[int(subject)] = text
        elif keyword == "assert":
            kind, point, rest = rest[0], int(rest[1]), rest[2:]
            reported_by = None
            if rest[:1] == ["from"]:
                reported_by, rest = int(rest[1]), rest[2:]
            assertions.append({"id": point, "kind": kind,
                               "reported_by_hook": reported_by,
                               "meaning": " ".join(rest)})
        elif keyword == "diagnostic":
            diagnostics.append({"name": rest[0], "argv": rest[1:]})
    for node in nodes:
        node["description"] = node_text.get(str(node["id"]))
    for hook in hooks:
        hook["description"] = hook_text.get(hook["id"])
    return {"format": "harmony-workload-declarations-v1", "nodes": nodes,
            "hooks": hooks, "assertions": assertions, "diagnostics": diagnostics,
            "setup": setup, "ready": ready}


def _observations(moment: int, violations: list[int]) -> dict:
    return {"moment": moment, "ticks": 96, "alive": 1, "hooks_started": 8,
            "hooks_finished": 8, "unexpected_deaths": 0, "restarts": 4,
            "parked": 0, "sometimes_register": 0,
            "sometimes": [21, 23, 25], "violations": violations,
            "stop": {"Assertion": {"point": 2}} if violations else "Deadline"}


def derive(destination: Path, repository: Path) -> dict:
    """Assemble a workspace from the historical case without executing a VM."""
    case = json.loads((repository / CASE / "case.json").read_text())
    witness = json.loads((repository / CASE / "witness.json").read_text())
    actions = witness["actions"]
    horizon = witness["horizon_nanos"]
    failure = horizon * len(actions)

    if destination.exists():
        shutil.rmtree(destination)
    (destination / "journal").mkdir(parents=True)
    (destination / "blobs").mkdir()

    blobs = {}
    for name, text in (("console", CONSOLE), ("events", EVENTS), ("command", AMCHECK)):
        data = text.encode()
        digest = _hex(data)
        (destination / "blobs" / digest).write_bytes(data)
        blobs[name] = (digest, len(data))

    facts = {
        "record": "facts", "format": FORMAT, "package": "faults",
        "identity": "faults-consonance-whole-vm-v1",
        # A derived fixture pins no artifact it can hash, and says so rather
        # than inventing a digest that no image would ever match.
        "image_sha256": "", "kernel_sha256": "", "fault_agent_sha256": "",
        "image": f"pgcic-{case['arms']['vulnerable']['version']}.oci",
        "root_seal": 0, "horizon_nanos": horizon, "ram_mib": 2048,
        "knobs": [], "seed": case["search"]["seed"],
        "executions": case["search"]["executions"],
        "declarations": _declarations(),
    }
    records = [facts, {
        "record": "moment", "id": "m-0001", "branch": None,
        "virtual_time": failure, "history": "recorded", "checkpoint": None,
        "state_hash": _hex(json.dumps(actions).encode()),
        "stop": {"assertion": {"point": 2}},
        "observations": _observations(failure, [2]),
    }, {
        "record": "finding", "id": "bug-1", "execution": 3187,
        "actions": actions, "standing": "",
        "observations": _observations(failure, [2]),
        "violations": [2], "evaluated": [2, 20, 21, 22, 23, 24, 25],
        "moment": "m-0001", "confirmed": True,
        "state_hash": _hex(json.dumps(actions).encode()),
    }]
    for index, (kind, moment) in enumerate(
        (("console", "m-0001"), ("events", "m-0001"), ("command", "m-0001")), start=1
    ):
        digest, size = blobs[kind]
        records.append({
            "record": "evidence", "id": f"ev-{index:04}", "kind": kind,
            "moment": moment, "blob": digest, "bytes": size, "truncated": False,
            "precision": "interval; console lines carry only the span they were "
                         "drained over" if kind == "console" else
                         "stream position; each report carries its position and "
                         "virtual time",
        })
    transaction = {"format": FORMAT, "sequence": 1, "records": records}
    (destination / "journal" / "000001.json").write_text(json.dumps(transaction, indent=1))
    return {"source": "derived", "workspace": str(destination),
            "finding": "bug-1", "property": 2, "hook": 3,
            "actions": len(actions), "case": case["id"]}


def adopt(recorded: Path, destination: Path) -> dict:
    """Copy a workspace a real search wrote."""
    if not (recorded / "journal").is_dir():
        raise SystemExit(f"{recorded} is not a Harmony workspace")
    if destination.exists():
        shutil.rmtree(destination)
    shutil.copytree(recorded, destination)
    return {"source": "recorded", "workspace": str(destination)}
