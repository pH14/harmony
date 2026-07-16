# Task 118 — Dissonance document/naming convergence (`hm-7zx`)

**Tier: light (docs only — no crate code, no renames).** Unblocked by Paul 2026-07-16
("please unblock the dissonance / renaming work — it can run concurrently with the
consonance work"). This is the gate for the Differential migration epic (`hm-bbx`):
its children (`hm-bbx.1` SDK normalization, `hm-bbx.2` lineage/evidence-cut spike)
surface in `bd ready` when this closes.

## Deliverable

Reconcile `docs/ROADMAP.md`, `DISSONANCE.md`, `EXPLORATION.md`, `RESOLUTION.md`,
`LAYERS.md`, `SCORING.md`, `GLOSSARY.md`, `QUEUE.md` (structure only — the foreman
regenerates content), and the historical task-84/task-86 spec text with
**`docs/DISSONANCE-STRATEGY.md`** (the ruled strategy, PR #103) and **current code**.
One pass, one PR, so the docs stop contradicting the strategy and each other.

Apply the settled names and boundaries (from the bead, verbatim):

- `conductor` → `campaign-runner`, with **counterpoint reserved** (GLOSSARY authority);
- recompute cells vs `EnvCodec` `Moment` shifting;
- state reduction/derivation vs `CellFnV1` quantization;
- provenance vs identity/value/cell projection;
- link to `sdk-events`; `GuestEvent` → `SdkEvent`; SDK catalog → normalized `SdkSchema`;
- record both LAYERS R-L3 ingress formats and any unresolved legacy declarations;
- record explicit evidence ordering/cuts, lineage-complete prefixes, retention and
  finalization boundaries, deterministic `Revision` assignment;
- record current spine dispositions.

**Do NOT perform big-bang code renames here** — physical migrations ride substantive
work (the bead says so; the rename-sweep precedent is tasks/105). Where a doc names
code that has not been renamed yet, state the ruled target name and the current code
name side by side rather than pretending either way.

## Notes

- `docs/ROADMAP.md` is ~2 weeks stale (Wave-4 table lists merged tasks 58–61 as "not
  yet started"). Bring it to current-state truthfully; today's landmarks: nested-x86
  re-certified (PR #98), contract vendor axis (PR #116), ARM skeleton near-merge
  (PR #117), CI migration in flight (PR #118), `guest/`→`harmony-linux/` filed as
  `hm-ciz` (P3, after the ARM merge).
- Cross-check every claim against the tracker (`bd list`) — the tracker wins.
- The known R-L4/task-43 material is already written in `docs/LAYERS.md` §R-L4; link,
  don't duplicate.

## Gates

Docs-only: markdown link check (repo convention), no crate builds required. The PR
description must list, per document, what was reconciled and what was left stale
deliberately (with why). Light-tier review (foreman sanity read; no cross-model pass
per the 2026-07-09 posture).

## Definition of done

Every named doc agrees with DISSONANCE-STRATEGY.md and the tracker on: names,
boundaries, what is built vs ruled vs deferred. `hm-7zx` closes on merge; the
Differential epic children surface in `bd ready`.
