#!/usr/bin/env python3
"""Prepare, then separately execute a fixed complete-survivor suffix diagnostic."""
import argparse
import json
from pathlib import Path

from run_p05 import CORE, ROM, run_bounded, sha

SEED, TRIALS, ACTIONS, WORK_CAP = 20261215, 16, 24, 4_000_000
PROBE_SHA = "20f0255bc4002d4e43ffecbfd9e0efc053c7656c2b06272bc4fbc16e7eff9672"
PREFERENCE = ["health", "missiles", "equipment", "bosses", "missile_capacity", "energy_tanks"]


def route_frames(value):
    # An empty genesis input still incurs setup; it is not an invalid route.
    assert len(value["actions"]) <= 8192
    frames = sum(max(1, min(120, a["hold_frames"])) for a in value["actions"])
    assert frames <= 250_000
    return frames


def work_bound(pairs):
    prefix = sum(route_frames(pair[k]) + 4096 for pair in pairs for k in ["candidate_input", "incumbent_input"])
    suffix = len(pairs) * 2 * TRIALS * ACTIONS * 120
    exports = TRIALS * sum(route_frames(pair["candidate_input"]) + 4096 + ACTIONS * 120 for pair in pairs)
    return {"prefix_upper_bound": prefix, "suffix_upper_bound": suffix,
            "gain_exports_upper_bound": exports, "total_upper_bound": prefix + suffix + exports}


def expand(pair, source_index):
    complete = pair["complete_competition"]
    if complete is None or len(complete["incumbents"]) != 2:
        return None
    states = [{"role": "candidate", "state": pair["candidate"], "input": pair["candidate_input"],
               "kept": complete["candidate_admitted"]}]
    states += [{"role": f"incumbent_{m['id']}", "state": m["state"], "input": m["input"],
                "kept": m["retained_by_local_rule"]} for m in complete["incumbents"]]
    if any(any(s["state"][k] != states[0]["state"][k] for k in PREFERENCE) for s in states):
        return None
    discarded, survivors = [s for s in states if not s["kept"]], [s for s in states if s["kept"]]
    if len(discarded) != 1 or len(survivors) != 2:
        return None
    discarded = discarded[0]
    rows = []
    for survivor in survivors:
        # The frozen pair probe requires these metadata fields but never reads
        # them. Zero placeholders are explicitly not exposure measurements.
        rows.append({"stratum": 3, "execution": pair["execution"], "replaces": False,
                     "candidate": discarded["state"], "incumbent": survivor["state"],
                     "candidate_input": discarded["input"], "incumbent_input": survivor["input"],
                     "created_execution": 0, "exposure": {"selected": 0, "productive": 0},
                     "in_window_ever": False})
    record = {"source_sample": source_index, "execution": pair["execution"],
              "discarded_role": discarded["role"], "survivor_roles": [s["role"] for s in survivors],
              "candidate_survivor_index": next((i for i, s in enumerate(survivors) if s["role"] == "candidate"), None),
              "route_frames": [route_frames(s["input"]) for s in states]}
    return record, rows


def prepare(root):
    qualification_path = root / "runs/u01-qualify/results.json"
    qualification = json.loads(qualification_path.read_text())
    assert qualification["passed"] and len(qualification["records"]) == 4
    paths = list((root / "runs/u01-qualify/quality").glob("*/campaign/retention-audit.json"))
    assert len(paths) == 1
    audit_path = paths[0]
    audit = json.loads(audit_path.read_text())
    assert audit["format"] == "metroid-retention-audit-v2-local-survivors"
    record = next(r for r in qualification["records"] if r["label"] == "quality")
    assert sha(audit_path) == record["audit_sha256"]
    assert sha(root / "assets/quicknes_libretro.so") == CORE and sha(root / "assets/metroid.nes") == ROM
    assert sha(root / "builds/probe-001/metroid-retention-probe") == PROBE_SHA
    for path in (root / "runs").rglob("summary.json"):
        assert json.loads(path.read_text()).get("seed") != SEED, "suffix seed already used"
    eligible = [expanded for i, pair in enumerate(audit["samples"][3]) if (expanded := expand(pair, i)) is not None]
    candidates = eligible[:8]
    # Use only a prefix in original sample order. Never skip an expensive early
    # competitor to substitute a later one, or select by suffix outcomes.
    while candidates and work_bound([row for _, rows in candidates for row in rows])["total_upper_bound"] > WORK_CAP:
        candidates.pop()
    out = root / "runs/u01"
    out.mkdir()
    pairs = [row for _, rows in candidates for row in rows]
    subset = out / "expanded-audit.json"
    subset.write_text(json.dumps({"format": "metroid-retention-audit-v1-pair-probe-adapter",
                                 "metadata_scope": "incumbent exposure/creation fields are unused zero placeholders, not measurements",
                                 "samples": [[], [], [], pairs, []]}, indent=2) + "\n")
    registration = {"format": "u01-frozen-suffix-registration-v1", "ready": len(candidates) >= 4,
                    "scope": "short seed3 development qualification; no fresh-search or population claim",
                    "source_audit_sha256": sha(audit_path), "qualification_sha256": sha(qualification_path),
                    "expanded_audit_sha256": sha(subset), "probe_binary_sha256": PROBE_SHA,
                    "seed": SEED, "trials_per_competition": TRIALS, "actions_per_trial": ACTIONS,
                    "eligible_competitions": len(eligible), "competitions": [r for r, _ in candidates],
                    "physical_work_cap": WORK_CAP, "work_bound": work_bound(pairs) if pairs else {},
                    "suffix_wall_seconds": 120, "output_cap_bytes": 64 * 1024**2,
                    "selection": "largest prefix of first eight eligible equal-preference samples that fits conservative bound; at least four required",
                    "qualifies_longer_search": False}
    (out / "registration.json").write_text(json.dumps(registration, indent=2) + "\n")
    print(json.dumps({"ready": registration["ready"], "eligible": len(eligible), "selected": len(candidates),
                      "work_bound": registration["work_bound"], "registration_sha256": sha(out / "registration.json")}), flush=True)


def execute(root, expected_registration_sha):
    out = root / "runs/u01"
    path = out / "registration.json"
    assert sha(path) == expected_registration_sha, "registration must be pinned before execution"
    registration = json.loads(path.read_text())
    assert registration["ready"]
    assert registration["work_bound"]["total_upper_bound"] <= WORK_CAP
    subset = out / "expanded-audit.json"
    assert sha(subset) == registration["expanded_audit_sha256"]
    assert sha(root / "assets/quicknes_libretro.so") == CORE and sha(root / "assets/metroid.nes") == ROM
    probe = root / "builds/probe-001/metroid-retention-probe"
    assert sha(probe) == PROBE_SHA
    destination = out / "suffix"
    assert not destination.exists()
    result_path = out / "results.json"
    assert not result_path.exists()
    result = {"format": "u01-suffix-execution-v1", "registration_sha256": expected_registration_sha,
              "complete": False, "build": json.loads((probe.parent / "build-info.json").read_text())}
    result_path.write_text(json.dumps(result, indent=2) + "\n")
    try:
        command = ["taskset", "-c", "8", str(probe), str(root / "assets/quicknes_libretro.so"),
                   str(root / "assets/metroid.nes"), str(subset), str(destination), str(TRIALS), str(ACTIONS),
                   "death_or_bcd_underflow_or_ending_v3", str(SEED)]
        result["elapsed_seconds"] = run_bounded(command, out / "suffix.log", out)
        summary = json.loads((destination / "summary.json").read_text())
        assert summary["seed"] == SEED and summary["audit_sha256"] == sha(subset)
        assert summary["core_sha256"] == CORE and summary["rom_sha256"] == ROM
        actual = summary["prefix_and_gain_export_frames"] + sum(summary["probe_frames_discarded_survivor"])
        assert actual <= registration["work_bound"]["total_upper_bound"]
        result.update({"summary": summary, "physical_frames": actual, "complete": True})
    except Exception as error:
        result["error"] = str(error)
        raise
    finally:
        result_path.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"complete": True, "physical_frames": actual}), flush=True)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--experiment", type=Path, required=True)
    p.add_argument("--phase", choices=["prepare", "execute"], required=True)
    p.add_argument("--registration-sha256")
    a = p.parse_args()
    if a.phase == "prepare":
        prepare(a.experiment.resolve())
    else:
        assert a.registration_sha256
        execute(a.experiment.resolve(), a.registration_sha256)


if __name__ == "__main__":
    main()
