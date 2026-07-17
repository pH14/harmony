# Task 132 — Full Differential vertical: production DD relations + SMB through the two-barrier controller (hm-e6q)

Claim `hm-e6q` first (`bd update hm-e6q --claim`). This is the box-gated follow-up Paul's
2026-07-17 ruling split out of hm-bbx.4 (tasks/130): the bounded Option-1 increment landed the
two-barrier `DifferentialCampaign` controller, the crash-safe `EvidenceLedger` (+ retention,
hm-5sv, PR #131), and the occurrence Oracle — but kept the Explorer's pure-function
recomputation as an honest stand-in and left campaign-runner's game loop bespoke. This task
makes the real differential-dataflow runtime the production backend and routes the actual SMB
workload through it.

**Read first, in full — these are the contract:** `bd show hm-e6q` (description + design +
acceptance_criteria), `docs/DISSONANCE-STRATEGY.md`, `bd show hm-bbx` (epic frame + the
McSherry doctrine: branch = key/dataflow-lifecycle, NEVER timestamp),
`dissonance/explorer/src/campaign.rs` (the two-barrier step you are rerouting onto),
`dissonance/revision-coordinator` (probe_drive/DrainedView/ledger — this surface does NOT
change), the PR-#124-F2 echo harness you are replacing, and `spikes/differential-lineage`
(the proven relations you are productionizing).

## Milestones (in order; commit and report each)

### M1 — Production DD relations inside ProbeHost
Replace the echo harness in revision-coordinator's `ProbeHost` dataflow graph with the
spikes/differential-lineage relations, so observations/cells/occupancy are materialized by
the real differential-dataflow runtime rather than recomputed in explorer. Hard constraints:
- The coordinator's `probe_drive`/`DrainedView`/ledger public surface is unchanged (public-api
  snapshot should not change for revision-coordinator's existing items).
- The McSherry doctrine is binding: branch identity is data (key) or dataflow lifecycle,
  never a timestamp.
- The Explorer's recomputation does not disappear here — it becomes the ORACLE: add a
  differential test that runs both and asserts view-for-view equality on every barrier-passed
  drain ("direct recomputation is an oracle, not a second backend"). This parity test is the
  M1 gate.

### M2 — SMB campaign through the two-barrier controller, box gates green
Route campaign-runner's `run_game_campaign` end-to-end through
`explorer::DifferentialCampaign`, replacing the bespoke BTreeSet/Vec Go-Explore-lite loop.
Then re-run the box gates on the determinism box (`ssh hetzner`, pinned per
`docs/BOX-PINNING.md`; ROM location: `bd memories smb-rom-location`):
- KVM game determinism **25/25** bit-identical, and the film gate (visible SMB clip via the
  task-87 projector path) — the same gates PR #93 established for M0.
- **Smoke-fire-once discipline**: before the full 25-run gate, run a minutes-long single-seed
  probe of the riskiest assumption (the rerouted campaign records + replays one seed
  bit-identically on real KVM) and report it.

### M3 — Retire the legacy spine
Once matcher/logtmpl/runtrace are migrated off `Explorer::step`/`Archive::admit` and the
compat spine (`Sensor`/`Feature`/`FeatureSet`/`CoverageArchive`/`IdentityCells`), physically
delete the legacy path (the acceptance criterion is "physically removed", not deprecated).
This is a 6+-crate blast radius — if migration surfaces a real design contradiction in a
consumer, STOP and escalate with the specific conflict rather than bending either side; if
the remainder of M3 is clean but large, land M1+M2 in the PR and propose the split explicitly
in the PR description for a foreman ruling.

## Gates & done

Full portable gates green (fmt, clippy --all-targets -D warnings, nextest, public-api
snapshots justified — expect intentional changes only where the legacy spine is deleted).
Mutation gate is live: kill surviving in-diff mutants with tests. Determinism is first-class:
same-seed end-to-end artifacts identical, and the M1 parity oracle + M2 box gates are the
decisive signals. Say explicitly what ran where (Mac vs box vs Linux CI). Open a PR mapping
each acceptance criterion to its test/gate evidence. `hm-e6q` closes on merge. Escalate (do
not guess) on any contradiction between the bead's acceptance criteria and
`docs/DISSONANCE-STRATEGY.md` — integrator ruling, not an implementer call.
