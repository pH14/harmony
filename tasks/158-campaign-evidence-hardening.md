# Task 158 — W7: PR #134 evidence + campaign bookkeeping hardening (hm-zduj)

**Work order:** `hm-zduj` (P0 epic, label `work-order`). Read it first —
`bd show hm-zduj` — then every child with `bd show <id>`. Each child carries the
exact file:line and its review provenance (tribunal finding or verify event on
PR #134); this spec is the shape of the work, not a restatement.

These ten beads are the ones the `hm-bbx` epic named by ID in its own closing note
as "open parked hardening beads from the child reviews remain individually tracked."
That sentence is the whole argument for this work order: the epic knew it was
orphaning them, and did it anyway, because review findings had no home. They do now.

**Surface:** `dissonance/campaign-runner/src/gamecampaign.rs`,
`dissonance/explorer/src/campaign.rs`, `dissonance/explorer/src/evidence.rs`,
`dissonance/revision-coordinator/src/host.rs`, `dissonance/sdk-events/src/schema.rs`.
Several of these are one commit apart from each other — that is why they travel
together.

## Reachability, stated once so you calibrate correctly

**None of these is reachable by an untrusted guest.** They require a forged on-disk
ledger, direct public-API misuse, a buggy trusted transport, or a test-only entry
point. That is not permission to hand-wave them: this project's entire output is
evidence integrity, and an evidence ledger whose identity function silently changed
is a correctness bug in the thing we sell. It *is* permission to prefer a clean
structural fix over a defensive-check sprawl, and to fail closed loudly rather than
paper over.

## Order — the choke point first, because three others fold into it

### 1. `hm-tx66` (F7) — one validated evidence/staging choke point

The anchor item. Build a single validated `stage_evidence` / ledger-open choke point
that enforces, in one place: lineage acyclicity (today a cyclic/self-parent lineage
hangs `probe_drive` — `host.rs:223-227`, distinct-over-depth never converges),
checked cumulative-position arithmetic with a typed error (`evidence.rs:379-389`;
`campaign.rs:557/1157/1306/1321`), intra-batch declare-conflict dedup
(coordinator gap), and input bounds. Also kill the O(N²) accumulated-prefix clone at
`host.rs:266-273` while you are in there.

**`hm-t5py` folds in here**: `gamecampaign.rs:678` increments a backend-stamped
cut's `sdk_events` without `checked_add` — a no-catalog `Machine` returning
`sdk_events == u64::MAX` panics in debug/Miri and wraps to zero in release, and a
wrapped cut retains inherited evidence. Same checked-arithmetic + typed-error
treatment, same choke point if it fits there naturally; a separate guard if forcing
it through the choke point distorts either one.

### 2. `hm-9zr5` (F5) + `hm-8deo` (V10) — the on-disk identity pair. Land together.

Both change durable identity, so they share one version bump. Doing them in separate
commits with separate bumps would be worse than doing them at all.

- **`hm-9zr5`**: `evidence.rs:291-298` — `parent_cut` + `sealable_moments`
  (serde-default) already changed `canonical_bytes()` / `EvidenceBatchId` while the
  ledger format stayed at version 2, so reopening a pre-patch v2 ledger silently
  recomputes different IDs. Bump the ledger version so a pre-patch ledger **loudly
  rejects** instead.
- **`hm-8deo`**: `gamecampaign.rs:849-850` digests only `campaign_seed`, violating
  the `ids.rs:114-121` contract that `CampaignConfigId` is the content-addressed
  identity of the *immutable campaign configuration*. Same-seed runs with different
  selectors/deadlines/caps currently collide on durable identity. Digest a canonical,
  **versioned** encoding of the full config.

Note the ordering hazard: fixing `hm-8deo` changes every `CampaignConfigId`, which is
itself a durable-identity break. The version bump from `hm-9zr5` must cover it. Say
in the PR body exactly which on-disk artifacts a v2 ledger holder loses and what the
rejection message tells them to do.

### 3. `hm-vfop` (F6) — reused `--trace-out` dir

`gamecampaign.rs:835-852`: a reused trace dir reopens the prior ledger while the
coordinator starts empty, so `DifferentialCampaign::new` rebuilds old occupancy and
the first step fail-closes with `OccupancyDivergence`. Fail-closed, not silent — but
it is a confusing failure for an honest mistake. Namespace or truncate the trace dir
per run, or make the fresh-dir requirement an enforced precondition with a message
that names the fix. **Distinct from the standing anomaly in item 8 — do not conflate
them in the PR description.**

### 4. `hm-dd39` (V11) — `DeclaredMachine` loses the raw guest v1 bytes

`gamecampaign.rs:664-676` rewrites a v1 catalog into a synthetic v2 declaration
before any decoder runs, while `sdk-events/src/schema.rs:17` promises
`original_declaration` is the original, for audit and migration. Semantics are
preserved (via `resolve_v1_declaration`) and it is deterministic — the defect is
that the audit promise is false. Retain the raw v1 bytes, or record explicit upgrade
provenance and fix the doc promise to match. Do not "fix" it by weakening the
schema's promise alone.

### 5. `hm-7h2c` (V9) — recovery silently skips committed batches

`explorer/src/campaign.rs:406-411`: a recovered coordinator lists committed batch IDs
whose relation rows `ledger.get()` no longer returns; the rows are silently omitted
and the next exploit can derive a wrong inherited cell or a spurious
`OccupancyDivergence`. Unreachable in shipped code (`run_game_campaign` always pairs
a fresh `MemLedger`; `Coordinator::recover` is test-only) — but silent omission in a
recovery path is the exact species this work order exists to kill. Same-config
durable-coordinator recovery must **refuse or reseed** on a missing batch.

### 6. `hm-7k8f` (F4) — widen the M1 parity oracle

`campaign.rs:1677-1721`: `view_pairs` filters to `r == rollout && p == point`, so a
phantom obs/cell row at a **never-staged** coordinate is never parity-checked.
Occupancy is full-view compared (`:1761`) and drift at staged coords is caught
bidirectionally, so the gap is bounded — but this is a gate auditor that cannot see
a whole class of wrong row. Widen it to detect rows at unexpected coordinates. Per
W1 doctrine: ship the planted-failure fixture that makes this oracle go red, or it
is not a gate.

### 7. `hm-w2ar` — the surviving mutant + five timeouts

Sharded mutants on PR #134 final head 26b9fb5c (run 29857330385): 1 MISSED —
`revision-coordinator/src/host.rs:471:18`, "replace `>` with `>=` in
`ProbeHost::drive`" survives the suite, i.e. an untested boundary. Add the boundary
test that kills it. Then triage the 5 timeout mutants (2 in shard 2, 3 in shard 3)
for slow-test artifacts and say what they were.

### 8. `hm-4vms` (F9) — **do not close this one**

The unreproduced intra-campaign occupancy divergence ("2 materialized vs 4 mirror",
one smoke attempt; all 25 gate reps and the next identical invocation clean). It is a
standing escalation and it does **not** gate this work order. Keep the diagnostics
armed — if any of your work would disarm or reroute them, say so loudly instead of
doing it quietly. If it reproduces at any point during this task, **stop and report
immediately**: it becomes a P0.

## Gates

Full nextest for `campaign-runner`, `explorer`, and `revision-coordinator`; clippy
`-D warnings`; fmt; `cargo mutants --in-diff` 0 missed (this task is partly *about*
mutant survival, so a missed mutant in your own diff is a self-inflicted wound).
Every behavior change ships its regression test in the same commit. The ledger
version bump ships a test proving a v2 ledger is rejected with the intended message.

If any change touches a public surface, check the frozen-API (`public-api`) snapshot
and regenerate on the pinned nightly if needed, flagging the delta in the PR body.

## Deliverable

PR from `task/campaign-evidence-hardening`, one commit per bead (or per landed pair
where the spec above says land together), closing with the merge: `hm-tx66`,
`hm-t5py`, `hm-9zr5`, `hm-8deo`, `hm-vfop`, `hm-dd39`, `hm-7h2c`, `hm-7k8f`,
`hm-w2ar`. `hm-4vms` stays open by design.

If an item turns out to be wrong — the finding misreads the code, or the fix would
cost more than the defect — **say so with evidence and leave it open**. A reasoned
"this finding is not real, here is the read that shows it" is a good outcome and
gets recorded on the bead. Silently skipping it is not.
