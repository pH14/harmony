#!/usr/bin/env python3
"""Render completed development milestone evidence; requires Matplotlib."""
import argparse
import hashlib
import json
from pathlib import Path
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.lines import Line2D


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--research", type=Path, default=Path(__file__).resolve().parent)
    a = p.parse_args()
    files = [a.research / "r04b-development-analysis.json", a.research / "r05-analysis.json"]
    data = [json.loads(path.read_text()) for path in files]
    colors = {"ordinary": "#0072B2", "quality": "#666666", "context": "#D55E00"}
    labels = {"ordinary": "Ordinary retention", "quality": "Two quality representatives", "context": "Two motion contexts"}
    milestones = [("missile_capacity", "Missile capacity"), ("norfair", "Norfair"), ("energy_tank", "Energy tank")]
    fig, axes = plt.subplots(1, 2, figsize=(11, 4.4), sharey=True)
    for ax, report, title in zip(axes, data, ["R04b · development seed 3", "R05 · fresh development seed 20261210"]):
        ceiling = report["common_frame_ceiling"] / 1e6
        assert ceiling == 50
        arms = [label for label in colors if label in report["arms"]]
        offsets = {label: (index - (len(arms) - 1) / 2) * .21 for index, label in enumerate(arms)}
        for label in arms:
            arm = report["arms"][label]
            assert arm["result"]["stop_reason"] != "wall_limit"
            for index, (name, _) in enumerate(milestones):
                y = len(milestones) - 1 - index + offsets[label]
                if name in arm["final_names_at_common_work"]:
                    interval = arm["milestone_frame_intervals"][name]
                    lo, hi = interval["lower_exclusive"] / 1e6, interval["upper_inclusive"] / 1e6
                    assert 0 <= lo <= hi <= ceiling
                    ax.errorbar(hi, y, xerr=[[hi - lo], [0]], fmt="o", color=colors[label], capsize=3, markersize=5)
                else:
                    ax.plot(ceiling, y, marker=">", fillstyle="none", color=colors[label], markersize=7)
        ax.set_title(title, fontsize=11, loc="left", pad=14)
        ax.set_xlim(0, 53)
        ax.set_xticks([0, 10, 20, 30, 40, 50])
        ax.set_xlabel("Admitted search frames (millions)", labelpad=10)
        ax.set_yticks([2, 1, 0], [label for _, label in milestones])
        ax.set_ylim(-.5, 2.5)
        ax.grid(axis="x", color="#dddddd", linewidth=.6)
        ax.set_axisbelow(True)
        ax.spines[["top", "right", "left"]].set_visible(False)
        ax.tick_params(axis="y", length=0)
    handles = [Line2D([], [], color=color, marker="o", linestyle="none", label=labels[label]) for label, color in colors.items()]
    fig.legend(handles=handles, loc="upper center", ncol=3, frameon=False, bbox_to_anchor=(.5, .91), fontsize=9)
    fig.suptitle("Motion retention: development comparisons at matched work", x=.06, ha="left", fontsize=14, fontweight="bold")
    fig.text(.06, .05, "Dots mark the latest telemetry arrival; whiskers show its interval. Open arrows: not observed by 50M frames.\n"
             "Replayed named milestones, same feature binary and 8 GiB budget. No boss result or held-out validation claim.", fontsize=9, color="#444444")
    fig.subplots_adjust(left=.13, right=.98, top=.75, bottom=.26, wspace=.2)
    for extension in ["svg", "png"]:
        fig.savefig(a.research / f"motion-development.{extension}", dpi=180, metadata={"Date": None} if extension == "svg" else None)
    provenance = {"format": "motion-development-figure-v1", "matplotlib": matplotlib.__version__,
                  "inputs": {path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in files},
                  "scope": "selected named development milestones; telemetry intervals, not confidence intervals"}
    (a.research / "motion-development-figure.json").write_text(json.dumps(provenance, indent=2) + "\n")


if __name__ == "__main__":
    main()
