# Ablate early entry exhaustion using the existing hard cap

RF01 closes the single-root controller line. This separate selection hypothesis
uses no resource injection, selected origin, new retention descriptor or action
operator. Change only the existing semantic-cost selector's entry cutoff from3
to64, the engine's compiled hard cap:

- Control: `room_cell_uniform_128_energy_progress_cheapest_v1:3,6,12,2`.
- Candidate: `room_cell_uniform_128_energy_progress_cheapest_v1:64,6,12,2`.

This is a configuration ablation, not a new production implementation or tuned
optimum. Keep the original cost ranks, cell novelty, pooled barren counters,
quarter-uniform fallback, retention, observations and independent action law.
The no-cost, action-correlation, local-retry and retention failures stay closed.

## Distinct causal change and existing evidence

`Archive::entry_unexhausted` removes a parent from the main group walk once its
consecutive nonproductive selection count reaches the configured cutoff. Every
ordinary admitted job increments that count; any retained descendant resets it.
The main walk gets three quarters of reservations. The uniform quarter can still
choose exhausted active parents. Global reset occurs when no eligible class
remains. These are soft population-level safeguards around a hard per-entry
cutoff; they do not imply a fixed per-parent sampling probability as the archive
grows. Pooled energy downweights groups, while count policies divide within-cell
weights. Neither changes this per-entry eligibility boundary.

The [retrospective audit](audit.py) reconstructs all2,548 AP01 jobs, keyed by stable
entry IDs. Its totals match the native report's768 productive selections and165
active entries over the cutoff. Metadata compaction removes444 historical entries;
it preserves counters of surviving IDs and occurs after outcome accounting.
Do not confuse those removals with the zero population `entry_drops` counter.

There are151 jobs admitted when their parent's reconstructed pre-admission streak
is at least3, including122 uniform and29 main-walk jobs. Thirty produce retained
descendants. Those29 main-walk cases are not policy violations: dispatch precedes
admission and bounded in-flight jobs can cross the cutoff. These counts neither
estimate fresh-search prevalence nor prove useful future progress. The already
identified HP129 entry982 has three main-walk failures followed by one uniform
failure; no successful continuation from that entry was observed. The archive
retained and revisited it. Its low exposure motivates an allocation test, not
an assertion that more exposure would defeat the boss.

## Exact finite tradeoff and executable falsifier

The [rational model](model.py) has two persistent parents, a main walk choosing
the preferred eligible class and the same quarter-uniform fallback. A failed
attempt from the preferred parent yields no retention; the other parent renews
an irrelevant same-class representative and stays eligible. Each attempt costs
one unit. Useful outcomes are independent at the stated fixed probabilities.
It omits native group energy, novelty, growth and scheduling; it is a constructed
counterexample, not an emulator probability model.

At16 attempts, when only the preferred parent succeeds with probability1/16 per
attempt, cutoff64 has hit probability0.59336 versus0.25338 for cutoff3. Reverse
which parent has that useful probability and cutoff64 falls to0.11794 versus
0.52033. Exact fractions, conservation at every step and an independent geometric
closed form for the unexhausted candidate are checked in [the results](model-results.json).
Neither cutoff dominates. A prior of rarity alone cannot determine the sign.

The actual Rust archive fixture round-trips both existing identifiers, verifies
identical pre-cutoff draws and random state, then checks the changed main-walk
eligibility after three failures. Both retain uniform access; the candidate still
exhausts at64 and productive credit reactivates it. This cheapest falsifier
passes. It changes tests only; existing pinned native evaluator binaries already
support both identifiers.

## Allocation decision

This earns a short, separately registered native qualification on the reused
Metroid fixture: full campaign/checkpoint replay, identical artifacts with one
versus two result buffers, and ARM/x86 semantic agreement. Reuse the qualified
milestone-stop binaries and exact assets; no rebuild or new observer is needed.
Stop qualification on identity, replay or resource failure; no automatic retry.

Only a pass earns a fresh paired development screen at first energy-tank
acquisition, whose50M-frame control horizon was already calibrated in C01.
Freeze four new pairs, ordinary cutoff3 versus64, the same4-worker/8GiB resources
and action/retention settings, balanced sequential arms on each host. Require
three strict wins and15% lower mean restricted milestone cost, robust to observed
intervals, with event-stopped CPU/wall ratios at most1.25. Two censored ties make
the gate impossible and stop dispatch. A failed gate closes this ablation with
no intermediate cutoff, new bank, longer horizon or secondary-endpoint rescue.
A pass earns independent confirmation and same-policy MM2 transfer before any
boss/Wily validation. No native allocation exists until separately registered.

The original repeated fresh boss/Wily goal remains active and unachieved. All
work stays in PR #287. The expired tranche and all completed ledgers stay frozen.

## EQ01 passed; ED01 is frozen before fresh dispatch

[EQ01](eq01-analysis.json) completes three2,000-job native fixtures at293,214
admitted frames each. Both ARM buffer variants have identical stream, campaign
and checkpoint bytes; ARM/x86 post-header event streams match exactly. Full
campaign/checkpoint replay and held witnesses pass. The processes take20.32,
19.18 and9.04 seconds and all services are terminal. Lossless evidence is in
`eq01-output/`; the verifier recomputes the same analysis from compressed files.
[Known auxiliary cost](ledger-after-eq01.json) is1,775,802 frames, with cumulative
resumed totals1,083,261,532 admitted search and60,275,093 known auxiliary frames.
Setup, unadmitted work and reconstruction remain separately unknown.

[ED01](ed01-registration.json) freezes four fresh paired seeds checked against
saved records on both hosts. Only cutoff3→64 differs within each pair. The
first wave runs pairs0/1 on msr1/ms02; pairs2/3 remain undispatched unless the
three-win gate is still attainable. Each arm has the calibrated50M energy-tank
horizon,1M-job/20-minute search ceiling,4 workers,8GiB logical archive,12GiB service
memory and4GiB output limit. Arms are sequential on the same CPUs with balanced
order. The complete block has400M nominal admitted frames plus bounded drain,
8M known auxiliary frames and an unchanged two-hour deadline, including all
completion time. Failed/incomplete cells are preserved and stop dispatch.

The scorer reuses the already tested four-pair resource/endpoint logic from the
persistence panel and maps only its historical decision labels. Those old
outcomes do not enter ED01. Existing five planted scorer tests pass; empty ED01
evidence remains incomplete and cannot pass. Registration commits precede all
fresh execution. A precursor win would still require independent confirmation
and MM2 transfer before untouched boss/Wily validation.
