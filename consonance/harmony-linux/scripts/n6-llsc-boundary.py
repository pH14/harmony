#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Executable proof of N0's bounded LL/SC admission decision."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class Outcome:
    """Guest-visible retained state after one logical atomic update."""

    value: int
    observed_retries: int


def update(failure_schedule: tuple[bool, ...], accumulating: bool) -> Outcome:
    """Run one LL/SC loop under an architecturally legal STXR schedule."""
    value = 41
    retries = 0
    for spuriously_failed in failure_schedule:
        candidate = value + 1
        if spuriously_failed:
            if accumulating:
                retries += 1
            continue
        value = candidate
        return Outcome(value, retries)
    raise ValueError("schedule never permits STXR success")


def main() -> int:
    quiet = (False,)
    noisy = (True, True, True, False)

    pure_quiet = update(quiet, accumulating=False)
    pure_noisy = update(noisy, accumulating=False)
    if pure_quiet != pure_noisy or pure_quiet != Outcome(42, 0):
        raise SystemExit("N6_LLSC_FAIL side-effect-free loop did not converge")
    print(f"N6_LLSC_POSITIVE_OK quiet={pure_quiet} noisy={pure_noisy}")

    accumulating_quiet = update(quiet, accumulating=True)
    accumulating_noisy = update(noisy, accumulating=True)
    if accumulating_quiet == accumulating_noisy:
        raise SystemExit("N6_LLSC_FAIL accumulating negative did not diverge")
    if accumulating_quiet.value != accumulating_noisy.value:
        raise SystemExit("N6_LLSC_FAIL negative changed the logical atomic result")
    print(
        "N6_LLSC_NEGATIVE_OK "
        f"quiet={accumulating_quiet} noisy={accumulating_noisy}"
    )

    # Planted comparator defect: comparing only the logical value would hide
    # exactly the retry-observing residue this decision excludes.
    if accumulating_quiet.value != accumulating_noisy.value:
        raise SystemExit("N6_LLSC_FAIL planted weak comparator unexpectedly failed")
    if accumulating_quiet == accumulating_noisy:
        raise SystemExit("N6_LLSC_FAIL full-state comparator accepted planted residue")
    print("N6_LLSC_COMPARATOR_NEGATIVE_OK weak=accepted full-state=rejected")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
