# S01: fixed paired selection screen

This is an exploratory allocation decision, not a powered efficacy trial.
Execution requires D02's native compatibility and candidate replay checks to
pass first. The new selection identifier and mathematical hypothesis are frozen
in [selection-hypothesis.md](selection-hypothesis.md). No coefficient is tuned.

The primary endpoint is the admitted-frame cost of first Metroid missile-capacity
acquisition, restricted to 25M frames. Preserve each observation interval. A
both-censored pair is a tie. The control and candidate use the same qualified
binary and features, fresh-game origin, ROM/core, 8 GiB archive budget, four
workers, CPU cores 8–11, action limit 4,096, one-to-six suffixes, alphabet-only
draws and ordinary retention. Only the selector identifier differs.

Four fresh seeds are derived from the first four SHA-256 bytes, big-endian, of
the UTF-8 labels `harmony-s01-selection-20260909-v1:0` through `:3`:
3770579311, 3870466989, 2087347095, 2292227086. None appeared in the owned run
records at the pre-registration audit. The runner checks again before execution.
Pairs run in that order, serial on the same CPU set. Control goes first in pairs
0 and 2; candidate goes first in pairs 1 and 3. Shared seeds do not imply shared
trajectories or guarantee variance reduction after adaptive divergence.

Every cell has a 25M admitted-frame ceiling, 500,000-execution ceiling, 900-second
search wall limit, 120-second finishing allowance, 1,050-second outer watchdog,
12 GiB process-group memory cap and 4 GiB output cap. Frame ceilings allow only
the existing bounded in-flight drain. Both frame and wall/CPU costs are recorded.
An infrastructure failure or an incomplete common horizon is not scored as a
budgeted algorithmic failure. A verified earlier victory can supply an attained
primary endpoint, but cannot make an otherwise unobserved endpoint complete.

An exploratory pass requires at least three strict wins (`candidate_upper <
control_lower`) and at least 15% lower mean restricted cost even under the
least favorable interval endpoints (`100*sum(candidate_upper) <=
85*sum(control_lower)`). Candidate total CPU seconds and elapsed seconds,
including verification, must each be at most 1.25 times control totals; this
fixed resource tolerance is an allocation choice. Archive capacities and hard
process/output limits remain matched. Qualitative gains cannot replace the
registered primary endpoint.

After each completed pair, stop if even winning every remaining pair cannot
provide three strict wins. Otherwise finish all four pairs; do not stop early
on a favorable partial result. A full panel costs at most 200M nominal admitted
frames plus bounded drain and separately charged replay. Freeze its binary,
registration and scorer hashes before starting it. No seed or endpoint changes
are allowed after inspecting candidate results.

A pass earns independent confirmation and bounded MM2 transfer, subject to the
remaining tranche budget. A failure permits one evidence-backed pivot to action
distribution or continuation reuse, not a series of cost-weight tuning variants.
Neither outcome is a demonstrated boss/Wily4 breakthrough. Untouched validation
remains unrun unless the unchanged candidate qualifies and the remaining budget
can finish that validation.
