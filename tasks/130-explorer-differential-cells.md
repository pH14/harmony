# Task 130 — Integrate generic Explorer with Differential cells + archive (hm-bbx.4)

Claim `hm-bbx.4` first (`bd update hm-bbx.4 --claim`). This is the **culmination** of the
Differential-migration epic (`hm-bbx`): it wires together everything the merged children
built into the generic Explorer, replacing the legacy Sensor/Archive path. Its blockers are
all merged — SDK normalization (`hm-bbx.1`, `dissonance/sdk-events`), the prefix/retention
spike (`hm-bbx.2`), the Revision coordinator (`hm-bbx.3`, `dissonance/revision-coordinator`),
and atomic seal-cut capture (`hm-bbx.6`, the `Reply::Snapshot` cut now on the control seam).
Clearing this unblocks `hm-5sv`, `hm-m78`, `hm-cs5` and the rest of the cascade.

**Read first, in full — these are the contract:** `bd show hm-bbx.4` (description + design +
**acceptance_criteria** + **notes**), `docs/DISSONANCE-STRATEGY.md` (the ratified ruling),
`bd show hm-bbx` (the epic frame), and `docs/GLOSSARY.md`. Study the merged infrastructure you
build on: the `Reply::Snapshot` evidence cut (control-proto), the revision-coordinator's
durable batch-identity/commit API, and the sdk-events normalized-evidence surface.

## Scope

Replace the production `Sensor`/`FeatureSet`/`Archive::admit` path in `dissonance/explorer`
with:
- A **crash-safe append-only campaign evidence ledger** (minimum full-retention production
  ledger, crash-safe append/replay, bridge to TraceStore payloads). **TraceStore is referenced
  backing, never the relational authority; a live ledger reference cannot be invalidated**, and
  TraceStore retention cannot delete a live reference.
- **Completed-run evidence submission** → the Revision coordinator commits a durable batch
  identity (the coordinator owns proposal/Revision ordering; do not reimplement it).
- **Differential provisional transitions at unsealed cuts**, read **non-authoritatively** and
  **only after the first barrier**.
- **Budgeted canonical materialization replay** (charge a replay budget).
- A **later candidate-seal revision**: hold a temporary server-cut seal, submit the later
  revision, and **keep it only after actual-seal occupancy passes the second barrier**.
- **CellFn at the actual server-captured evidence cut / `sealed_at`** (use the hm-bbx.6 cut —
  the included-count prefix, never a Moment comparison).
- **Deterministic best-Entry-per-cell occupancy**; **temporary seal keep/drop**.
- A **pure Explorer-layer occurrence-counterexample Oracle** over one **borrowed immutable
  completed-run evidence view** (received after append — NOT a second mutable-ledger interface
  or duplicate event authority), plus **finalized property-level absence expectations**.
- **DELETE from production**: `LinkSensor`, LINK channels, packed register/value `FeatureId`.

## Hard invariants (from the acceptance criteria — all must hold)

- The **two-barrier protocol** in one Explorer step: durably append finished normalized
  evidence → submit the durable batch identity for commit → (barrier 1) read non-authoritative
  transitions → dedupe/order/cap candidates → charge replay budget → hold a temporary
  server-cut seal → submit its later revision → (barrier 2) keep only after actual-seal
  occupancy passes.
- **Restart** rebuilds canonical inputs and views from the ledger + referenced immutable
  payloads; **partial/uncommitted batches cannot advance a frontier**.
- **No provisional transition can occupy the archive**; disappearing pre-seal state is not
  admitted.
- Stable-quality and **Entry eviction are deterministic and separate from evidence retention**.
- Binary-terminal and JSON **occurrence counterexamples** use the immutable completed-run view
  and **dedupe by property**; **site coverage stays separate**; sometimes/reachable **absence**
  uses finalized property aggregates and retention-stable counts.
- The **legacy mutable archive path is unreachable** after this lands.
- The generic Explorer remains the **imperative campaign controller — Differential never
  schedules VM actions**.
- **End-to-end same-seed artifacts are identical** (the determinism gate).

## Notes (binding)

- **Absorbs `hm-mcx` as a regression**: evidence emitted at or after the terminal/crash
  boundary cannot influence a cell committed at an earlier seal. Test this explicitly.
- The current **serial-console adapter is full-run-only** (source-local, stop-granular) — it
  cannot drive exact same-Moment cuts, cross-source sequences, or log-derived CellFn dimensions
  at `sealed_at`. Do not promote it here; that needs capture-time serial stamps + a snapshot
  cursor + a shared machine-event ordinal (out of scope, note it).
- **Branch ingestion** appends only SDK-vector positions **after the parent cut** under the
  child rollout identity; the restored ancestor prefix is inherited through lineage and **must
  never be duplicated as child evidence**.

## Gates & done

This is a large cross-crate change on a determinism-critical path — treat gates as first-class.
Full portable gates green (fmt, clippy --all-targets -D warnings, nextest, public-api snapshots
justified). Any `unsafe` ⇒ Miri with an interpreter-reachable path. The **same-seed identical
artifacts** property gets an explicit end-to-end test. Because it touches the vmm-core/explorer
determinism surface, run the determinism-relevant portion on the box (`ssh <det-box>`, pinned
per `docs/BOX-PINNING.md`) or confirm Linux-CI coverage, and say what ran where. Open a PR with
a review-grounding description mapping each acceptance-criterion invariant to its test.
`hm-bbx.4` closes on merge, unblocking the cascade. Escalate (do not guess) on any contradiction
between the acceptance criteria and `docs/DISSONANCE-STRATEGY.md` — that is an integrator ruling.
