#!/usr/bin/env python3
"""Score a fixed paired screen using conservative interval bounds."""


def endpoint_cost(record, budget, condition=None):
    evidence = record["endpoint_evidence"]
    if evidence.get("milestone_stop") != condition:
        raise ValueError("endpoint evidence differs from the registered milestone stop")
    if condition is not None and (evidence.get("cost_convention") != "first_admitted_job_frames_v1"
                                  or evidence.get("endpoint") != condition["name"]):
        raise ValueError("registered milestone requires its exact admitted-job cost")
    assert evidence["budget_frames"] == budget
    if evidence["restricted_cost_interval"] is None:
        raise ValueError("incomplete endpoint evidence cannot enter a scored pair")
    lower, upper = evidence["restricted_cost_interval"]
    assert 0 <= lower <= upper <= budget
    assert evidence["observed_full_budget"] or evidence["hit_by_budget"] is True
    return lower, upper


def score_screen(records, registration):
    rule = registration["screen"]
    by_id = {record["id"]: record for record in records if record.get("checks_passed")}
    assert len(by_id) == sum(bool(record.get("checks_passed")) for record in records)
    pairs = []
    for pair in rule["pairs"]:
        if pair["control"] not in by_id or pair["candidate"] not in by_id:
            break
        control = endpoint_cost(by_id[pair["control"]], rule["budget_frames"], rule.get("milestone_stop"))
        candidate = endpoint_cost(by_id[pair["candidate"]], rule["budget_frames"], rule.get("milestone_stop"))
        pairs.append({"seed": pair["seed"], "control": control, "candidate": candidate,
                      "strict_win": candidate[1] < control[0],
                      "difference_interval": [candidate[0] - control[1], candidate[1] - control[0]]})
    wins = sum(pair["strict_win"] for pair in pairs)
    remaining = len(rule["pairs"]) - len(pairs)
    assert len(rule["pairs"]) == 4 and rule["required_strict_wins"] == 3
    decision = "continue"
    ratio = None
    resource_ratios = None
    if remaining == 0:
        control_sum = [sum(pair["control"][side] for pair in pairs) for side in (0, 1)]
        candidate_sum = [sum(pair["candidate"][side] for pair in pairs) for side in (0, 1)]
        assert control_sum[0] > 0
        ratio = [candidate_sum[0] / control_sum[1], candidate_sum[1] / control_sum[0]]
        # Cross multiplication avoids a floating-point decision at the threshold.
        improvement = 100 * candidate_sum[1] <= 85 * control_sum[0]
        decision = "pass" if wins >= 3 and improvement else "fail"
        if "resources" in rule:
            totals = {arm: {metric: 0.0 for metric in ("cpu_seconds", "elapsed_seconds")}
                      for arm in ("candidate", "control")}
            for pair in rule["pairs"]:
                for arm in totals:
                    summary = by_id[pair[arm]]["summary"]
                    for metric in totals[arm]:
                        assert summary[metric] > 0
                        totals[arm][metric] += summary[metric]
            resource_ratios = {metric: totals["candidate"][metric] / totals["control"][metric]
                               for metric in totals["control"]}
            if any(value > rule["resources"]["max_candidate_to_control_ratio"]
                   for value in resource_ratios.values()):
                decision = "fail_resource_gate"
    elif wins + remaining < rule["required_strict_wins"]:
        decision = "fail_impossible_win_count"
    return {"decision": decision, "completed_pairs": len(pairs), "strict_wins": wins,
            "remaining_pairs": remaining, "candidate_to_control_cost_ratio_interval": ratio,
            "candidate_to_control_resource_ratios": resource_ratios,
            "pairs": pairs, "interpretation": "Exploratory compute-allocation gate, not statistical significance or independent confirmation."}
