# Task 131 — Campaign evidence retention + completeness policy (hm-5sv)

Claim `hm-5sv` first (`bd update hm-5sv --claim`). This is a DD-epic cascade child, now
unblocked by the merged evidence ledger (hm-bbx.4, PR #130) and revision coordinator
(hm-bbx.3). It replaces the old unconditional-record-only runbook rule with the strategy's
explicit retention contract.

**Read first, in full:** `bd show hm-5sv` (description + **acceptance_criteria** + notes),
`docs/DISSONANCE-STRATEGY.md` (the retention contract), and the merged evidence ledger in
`dissonance/explorer` (evidence.rs / campaign.rs — the crash-safe append-only ledger + the
committed Entry cells this policy governs). Note the bead's provenance: tasks/97 / hm-b3h
flagged this as a "Known gap" in CORRELATION-REPORT.md; this bead is the forward-looking
discipline half so the gap is never re-created.

## Scope

- **Retention profiles.** `CampaignConfig` identifies the **retention profile** and its stable
  tie-breaks. Provide a **full-retention** evaluation profile (records from the *first* rollout)
  and **declared bounded** profiles.
- **What persists while retained:** immutable evidence, deterministic working-set
  membership/retractions, committed Entry assignments, finalized summaries, and
  completeness/loss metadata. Retained Entries keep **genesis-complete reproducers + lineage**.
- **Bounded expiry** updates only **working views** — it can **never** retract a live Entry
  cell or a finalized metric.
- **Ledger-aware physical GC.** Any TraceStore journal downgrade or deletion must **prove
  reachability + checkpoint coverage first**: a reference reachable from the evidence ledger or
  a live Entry **cannot be invalidated**. GC either leaves a rebuildable checkpoint or an
  explicit end to future reinterpretation.

## Hard invariants (acceptance criteria — all must hold, each with a test)

- `CampaignConfig` carries the retention profile + stable tie-breaks; full-retention runbooks
  record from the first rollout.
- Bounded expiry cannot retract a live Entry cell or a finalized metric (only working views).
- A TraceStore/raw-payload reference reachable from the ledger or a live Entry **cannot be
  invalidated**; GC proves reachability + checkpoint coverage before any downgrade/delete.
- Reports state **exactly** which raw evidence, derivations, and future recomputation remain
  available.
- **Host disk pressure cannot silently change policy — exhaustion fails LOUDLY** (never a
  silent policy downgrade).
- **Rebuild from a supported checkpoint matches live state** (bit-identical).

## Gates & done

Full portable gates green (fmt, clippy --all-targets -D warnings, nextest, public-api snapshots
justified). The mutation gate is now live again (hm-jfw fixed) — expect `cargo mutants` to run
on your diff; kill any surviving mutants with tests rather than scoping them out. Determinism
is first-class: the checkpoint-rebuild-matches-live-state property gets an explicit test, and
same-seed retention artifacts must be identical. If any Linux/KVM-only path is touched, run it
on the box or confirm Linux-CI coverage and say which ran where. Open a PR mapping each
acceptance-criterion invariant to its test. `hm-5sv` closes on merge. Escalate (don't guess) on
any contradiction between the acceptance criteria and `docs/DISSONANCE-STRATEGY.md` — integrator
ruling, not an implementer call.
