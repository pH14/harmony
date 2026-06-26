# Task 92 — multi-CPU characterization registry (probe → select → validate)

> **DEFERRED FOLLOW-ON · DO NOT AUTO-SPAWN.** Slot in **after the current queue clears** (the
> dissonance wave 24/25/26/12, task 28's box proof, pv-net). Medium priority — ahead of task 93.
> Not a freeze-window chore; a normal engineering task, just sequenced behind the active queue.
> Numbered 92 to keep it out of the day-to-day auto-spawn flow until then.

Read `tasks/00-CONVENTIONS.md`, `docs/CPU-MSR-CONTRACT.md`, and `tasks/11-rebaseline-cfl.md` first.
Background: the [[hardware-characterization-abstraction]] direction.

## Why

Today the CPU/MSR contract is a **single** baseline — `det-cfl-v1` (Coffee Lake, the 9900K box),
re-baselined from `det-skx-v1` (Skylake-X) in task 11. Re-baselining *replaced* one with the other.
The better shape is a **registry of named per-CPU characterizations** kept in-tree
(`det-skx-v1`, `det-cfl-v1`, …), with the engine **probing the host at startup, selecting the
matching characterization, and validating it** — so the determinism platform runs correctly on
more than one CPU without a destructive re-baseline each time, and a host it doesn't recognize
fails loud instead of silently using the wrong contract.

## Scope

- **Characterization registry.** A versioned, in-tree set of named characterizations, each the
  full §6 canonical form (kernel-tag/CPUID/MSR tables) + its `contract_hash`. `det-cfl-v1` is the
  current one; **recover `det-skx-v1` from git history** (task 11 replaced it — it is recoverable,
  do not re-measure) as the second entry.
- **Probe + select.** At startup, probe the host (CPUID signature / model) and select the matching
  characterization deterministically. No match ⇒ **loud fail** (never fall back to a wrong contract).
- **Validate.** Run the §1.1 host-assert for the *selected* characterization (the existing
  `host_assert_report` mechanism) and confirm it passes on that host before any determinism run.
- **Determinism-neutral.** The selected characterization's `contract_hash` is what flows into any
  hashed/golden artifact — selecting by host must not change a *given* host's hash. Two hosts use
  two characterizations; each host is internally stable. Grep the hash inputs to confirm the
  registry plumbing adds no new per-host input beyond the already-hashed canonical form.

## Acceptance gates

1. Registry holds ≥2 characterizations (`det-skx-v1` recovered + `det-cfl-v1`), each with its
   committed canonical form + `contract_hash`; a unit/property test pins each hash.
2. Probe→select is deterministic and **fails loud** on an unrecognized host (tested with a
   synthetic/mocked CPUID signature).
3. On the box (9900K): probe selects `det-cfl-v1`, the §1.1 host-assert passes, and M1/M2 + the
   det-corpus box gate are byte-identical to today (no determinism regression from the refactor).
4. Standard gates green; `contract_hash` for the box host unchanged (the refactor is a re-org,
   not a re-baseline).

## Non-goals

Re-measuring SKX (recover from git); adding a *third* CPU (the registry just has to support N);
the ARM characterization (that rides `docs/ARM-PORT.md` + the Phase 0.5 spike, separate). Do not
start until the current queue is clear.
