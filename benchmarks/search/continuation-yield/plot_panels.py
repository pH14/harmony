#!/usr/bin/env python3
"""Plot the three frozen paired panels. Requires matplotlib; no emulator work."""
import hashlib
import json
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.lines import Line2D
from matplotlib.ticker import FuncFormatter


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    root = Path(__file__).resolve().parent
    panels = [("s01", "Metroid · development", "First missile capacity"),
              ("r01", "Metroid · confirmation", "First missile capacity"),
              ("t01", "MM2 · transfer", "First stage victory")]
    plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 10,
                         "axes.spines.top": False, "axes.spines.right": False,
                         "axes.spines.left": False})
    fig, axes = plt.subplots(1, 3, figsize=(13.8, 5.4))
    provenance = {"matplotlib_version": matplotlib.__version__, "panels": {}}
    for ax, (stage, title, endpoint) in zip(axes, panels):
        result_path = root / f"{stage}-results.json"
        registration_path = root / f"{stage}-registration.json"
        analysis_path = root / f"{stage}-analysis.json"
        results = json.loads(result_path.read_text())
        registration = json.loads(registration_path.read_text())
        analysis = json.loads(analysis_path.read_text())
        assert analysis["results_sha256"] == sha(result_path)
        assert analysis["registration_sha256"] == sha(registration_path)
        score = analysis["score"]
        assert score["decision"] != "continue"
        records = {row["id"]: row for row in results["records"]}
        budget = registration["screen"]["budget_frames"] / 1e6
        pairs = registration["screen"]["pairs"]
        for index, pair in enumerate(pairs):
            present = [pair[arm] in records for arm in ("control", "candidate")]
            assert present[0] == present[1], "partial pairs need separate presentation"
            if not present[0]:
                ax.text(0.45 * budget, index, "not run", color="#64748b",
                        ha="center", va="center", fontstyle="italic")
                continue
            endpoints = [records[pair[arm]]["endpoint_evidence"]
                         for arm in ("control", "candidate")]
            centers = [sum(e["restricted_cost_interval"]) / 2e6 for e in endpoints]
            ax.plot(centers, [index, index], color="#cbd5e1", linewidth=2, zorder=1)
            for arm, evidence, center in zip(("control", "candidate"), endpoints, centers):
                assert evidence["hit_by_budget"] in (True, False)
                lower, upper = [x / 1e6 for x in evidence["restricted_cost_interval"]]
                color = "#475569" if arm == "control" else "#2563eb"
                marker = "o" if evidence["hit_by_budget"] else ">"
                ax.errorbar(center, index, xerr=[[center - lower], [upper - center]],
                            fmt="none", ecolor=color, capsize=2, zorder=2)
                ax.plot(center, index, marker=marker, linestyle="none",
                        markersize=9 if arm == "control" else 5.5,
                        markeredgecolor=color,
                        markerfacecolor="white" if arm == "control" else color,
                        markeredgewidth=1.5, zorder=3 if arm == "control" else 4)
        decision = "PASS" if score["decision"] == "pass" else "FAIL"
        count = f"{score['strict_wins']}/{score['completed_pairs']} completed pairs won"
        ax.set_title(f"{title}\n{decision} · {count}", loc="left", fontsize=11, pad=14)
        ax.set_yticks(range(4), [str(pair["seed"]) for pair in pairs], fontsize=8)
        ax.tick_params(axis="y", length=0, pad=7)
        ax.set_ylim(3.65, -0.75)
        ax.set_xlim(-0.015 * budget, 1.065 * budget)
        ax.set_xticks([budget * i / 5 for i in range(6)])
        ax.xaxis.set_major_formatter(FuncFormatter(lambda value, _: f"{value:g}"))
        ax.grid(axis="x", color="#e2e8f0", linewidth=0.8)
        ax.axvline(budget, color="#94a3b8", linestyle=":", linewidth=1)
        ax.set_xlabel(f"{endpoint}\nRestricted cost (million admitted frames)", fontsize=9)
        ax.spines["bottom"].set_color("#cbd5e1")
        provenance["panels"][stage] = {"results_sha256": sha(result_path),
                                         "registration_sha256": sha(registration_path),
                                         "analysis_sha256": sha(analysis_path)}
    fig.suptitle("Selection-cost ablation: paired discovery cost", x=0.05, y=0.97,
                 ha="left", fontsize=17, fontweight="bold")
    fig.text(0.05, 0.902, "Lower cost is better. Each panel retains its own endpoint and horizon.",
             color="#475569", fontsize=10)
    legend = [Line2D([], [], color="#475569", marker="o", markerfacecolor="white",
                     linestyle="none", markersize=8, label="Control"),
              Line2D([], [], color="#2563eb", marker="o", linestyle="none",
                     markersize=6, label="Candidate"),
              Line2D([], [], color="#475569", marker=">", markerfacecolor="white",
                     linestyle="none", markersize=8, label="Censored at the frame cap")]
    fig.legend(handles=legend, loc="lower left", bbox_to_anchor=(0.042, 0.075),
               ncol=3, frameon=False, fontsize=10)
    fig.text(0.05, 0.035,
             "Points use timing-bracket midpoints; whiskers bound observation timing, not statistical uncertainty.\n"
             "Nested markers show both-censored ties. The fourth MM2 pair was not run after the fixed gate became impossible.",
             fontsize=8.5, color="#475569")
    fig.subplots_adjust(left=0.095, right=0.98, top=0.78, bottom=0.25, wspace=0.52)
    fig.savefig(root / "paired-panels.png", dpi=180, facecolor="white")
    provenance["plotter_sha256"] = sha(Path(__file__))
    (root / "paired-panels-provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
    print(root / "paired-panels.png")


if __name__ == "__main__":
    main()
