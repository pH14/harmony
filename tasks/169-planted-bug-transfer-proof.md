# Task 169 — hm-ebe: prove cooperative Differential search on a planted software-system bug

> **FRONTIER · GO/NO-GO gate.** Paul recorded GO on `hm-yjf` (2026-07-28 01:09): the maze
> win transfers to a held-out test, not a proven claim. This lane IS that test. Its report
> feeds `hm-zlx` (transfer ratification — a Paul decision). A FAIL here is a recorded
> strategy NO-GO that blocks selector promotion; **it is not repaired by selector
> cleverness**, and a failed gate is not permission to iterate the strategy in this lane.

**Bead:** `hm-ebe` (P1). Read it first (`bd show hm-ebe` from the main workspace — the
worktree JSONL is a stale export). Its acceptance criteria are the contract; this spec adds
the workload decision, the lane discipline, and the stop conditions.

Read next: `tasks/60-first-campaign-planted-bug.md` (the planted-bug discipline this
extends), `docs/history/IMPLEMENTATION-task134.md` (the maze mechanism being transferred),
`docs/SCORING.md` (predeclaration doctrine), the PR #166 lane record (the first real
`/dev/harmony` transaction — your evidence path), `docs/GLOSSARY.md` + `docs/LAYERS.md`.

## The workload (foreman decision — build this, don't redesign it)

A **new, small, deterministic consensus-style guest toy** (single static binary is fine; an
in-process simulation of N logical replicas is fine — no real network needed) with bounded
**role / term / commit-index** state, instrumented through the **declared SDK evidence
path over `/dev/harmony`** (the Antithesis SDK surface, PR #78 ruling; the bridge-image
pattern from tasks/157 is your build template). It lives beside the other guest payloads
(`harmony-linux/linux/` build script + `bugs/toys/` entry documenting it per the bugs/
taxonomy).

**The planted bug**: a distributed-systems-species defect (e.g. a commit-index regression
or term/role invariant break) that is **reachable only under injected adversity** (the
fixed fault/tactic regime), **deterministically triggerable** given the right
(seed, schedule), **never fires under nominal conditions**, and is **observed by the
deterministic Oracle** via a declared SDK `always`-style assertion (not serial-text
scraping — the point of this lane is the SDK evidence path). Document the bug, its exact
trigger, and expected naive time-to-find in the toy's IMPLEMENTATION notes; keep it
findable within a box-scale budget (task-60 discipline: ~10²–10³ branches or a tunable
threshold).

## The mechanism under test (fixed — the hm-cs5 configuration, nothing fancier)

Generic Explorer + Differential cells/occupancy keyed on the **declared bounded
(role, term, commit) state**, the simple selector, fixed tactic/fault regime, exact
reproducer, deterministic Oracle. **No LLM, no advanced selector, no Portfolio, no bespoke
campaign loop** — the same interfaces `hm-cs5` used. If the declared-evidence stream
cannot produce well-formed cells, **STOP and report** (task-69 M2 ruling: if the sensor
can't make cells, stop — do not invent a count-based fallback in this lane).

## Predeclaration (M0 — commit BEFORE any campaign spend)

An M0 commit predeclaring: the budget B (seeds × branches), the bug/progress metrics
(bug find rate; censored time-to-bug and branches-to-bug; cells and progress), the PASS
threshold ("materially improve over pure random" made numeric BEFORE the runs), and the
three arms — archive-guided, equal-budget pure-random, nominal control. The maze lane's
report shape is the template. Changing any of these after first campaign data exists is a
spec violation, not a judgment call.

## Box discipline

`ssh hetzner`; coordinator `/root/box-window.sh` (time+pid leases — size `--ttl`, renew
before deadline on long campaigns); pin per `docs/BOX-PINNING.md`. **Smoke-fire-once
before campaign spend**: boot the toy image, prove one real SDK transaction end-to-end,
and force the planted trigger once via a hand-picked schedule (proving the bug + Oracle
fire at all) — report all three before the predeclared budget runs. Solo box tenancy;
box back to stock (1396736, zero leases, by hand) at every session end.

## Acceptance (from the bead, restated as gates)

1. Checked-in multi-seed report: workload/config/image hashes, cells and progress, bug
   find rate, censored time/branches-to-bug, all three arms at equal budget.
2. **Replay 25/25** bit-identical for the found bug's reproducer.
3. Nominal control never fires the bug; pure-random arm at equal budget for the
   comparison the PASS threshold names.
4. PASS ⇔ the simple archive-guided configuration finds the planted bug under the
   predeclared budget AND materially improves the predeclared metric over pure random
   without determinism loss. Anything else is a recorded NO-GO with the numbers — an
   honest NO-GO is a fully successful lane outcome; do not soften it.

## Stop conditions (halt and report; do not improvise)

- The SDK/bridge evidence path cannot carry the declared state (cells malformed/empty).
- The planted bug fires under nominal conditions (bug not adversity-gated — workload
  defect, not a search result).
- Solo-vs-co-tenant divergence or any determinism loss (P0 STOP, standing rule).
- Budget exhausted with ambiguous results — report as-is; the NO-GO framing is decided
  at `hm-zlx`, not by this lane.

## Gates

Portable: workspace nextest + clippy `-D warnings` + fmt for everything Mac-compilable;
every new gate W1-armed (shown able to fail). Box: the smoke trio, then the predeclared
campaigns. This is likely a multi-session lane — commit evidence promptly and leave a
clean handoff at every stop.
