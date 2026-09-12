"""Restrict an evaluator's exact admitted victory cost to a fixed budget."""


def victory_endpoint(result, budget):
    frames = result["frames_emulated"]
    arrival = result["frames_to_first_victory"]
    assert frames >= 0 and budget > 0
    assert arrival is None or 0 <= arrival <= frames
    complete = frames >= budget
    if arrival is not None and arrival <= budget:
        hit, cost = True, [arrival, arrival]
    elif complete:
        hit, cost = False, [budget, budget]
    else:
        hit, cost = None, None
    return {"endpoint": "first_stage_victory", "budget_frames": budget,
            "arrival_exact": arrival, "observed_through_frames": frames,
            "observed_full_budget": complete, "hit_by_budget": hit,
            "restricted_cost_interval": cost}
