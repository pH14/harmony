# QUEUE — what's moving, what's ready, what's blocked, and why

> Foreman-maintained dashboard, regenerated each loop iteration from the beads tracker
> (`bd ready` / `bd list`; issue IDs are `hm-*`). Descriptive names first, numbers as
> anchors. If this file disagrees with `bd list`, the tracker wins and this file is stale.
> Adopted 2026-07-09 (Paul: "worth a try") to replace prose-trigger sprawl across GitHub
> issues, task-spec headers, and memory notes.

_Refreshed 2026-07-23 midday (foreman loop): PR #147 MERGED through the full bounded
pipeline (discovery → fix → verify → fix → Closer re-check, zero open P1s) and PR #148
(the tasks/145 residue landing) MERGED light-tier; `hm-aqf0`/`hm-udgn`/`hm-nsfl` closed;
no active workers — next spawns from `bd ready` at the coming iteration._

Decision-gate safety: before dispatching ready work, the foreman inspects any closed decision
blocker and requires a recorded GO; it never dispatches in the same iteration that closes that
decision. NO-GO repairs or supersedes downstream edges before `bd ready` is used for dispatch.

## The Consonance north star (ruled by Paul, 2026-07-12)

**Consonance running in as many places as possible**: the reach matrix of vendors
(Intel / AMD / ARM) × forms (bare metal / virtualized). Intel×metal ships today;
Intel×virtualized RE-CERTIFIED 2026-07-16 (PR #98). **ARM×virtualized (Altra/N1): the full
AA-0..AA-6 ladder is now merged and closed** (`hm-idb` closed 2026-07-22) — AA-3 GO on the
regenerated-pin basis (PR #140, Paul-ruled), AA-6 injection PROVISIONAL GO ratified
(PR #139); the PROVISIONAL→full-GO upgrade rides the masked-register-digest named condition
(`hm-3bwm`, in flight — on-silicon leg parked, ARM box spun down 2026-07-22). AMD = AE
ladder pending the Epyc box (`hm-5wq` provider pick open).

## Decisions waiting on Paul

- **Ratify the cooperative mechanism vertical** (`hm-yjf`, P1): unblocked by the merged
  maze gate (PR #137). A recorded GO opens the software-system transfer chain (`hm-ebe` →
  `hm-zlx`) — the next Dissonance frontier.
- **PR #141 costed options** (§4 of `docs/history/IMPLEMENTATION-task139.md`): mutants
  runner-class residual — recommendation is ship the $0 option, prefer 8-way sharding
  later, reserve self-hosted.
- **`ci/gha-migration` un-shards the mutants job** (PR #141 §3 finding): survivability
  regression — weigh keeping sharding when landing `oob/gha-residue`.

## In flight

- (no active workers — 2026-07-23 evening; next spawns from `bd ready`: the PR #147
  park family is fully drained; remaining P2 frontier = `hm-w1o6` (interior-seal
  alignment design, now carries PR #155 V1's catalog-handling annotation), `hm-4gaw`,
  `hm-f82p`, `hm-5mx0` (v1-verb test machine), `hm-0qpm` (doc one-liner))
- **Parked box lane**: `hm-3bwm` masked-register-digest ≥1000-rep on-silicon leg —
  apparatus + turnkey runbook MERGED (PR #142); fires when an ARM window reopens
  (`hm-x9f` or a re-lease). All-identical ⇒ escalate the full-GO upgrade to Paul.
- **Out-of-band: GHA-migration residue** (`oob/gha-residue`, pushed): the evacuated
  `ci/gha-migration` work — hosted-runner content-check gates + cargo-deny pin (the two
  unique ci commits), provider-neutral skills (harmony-coordinator/handoff/pr-review/
  nimbus), secret-hygiene stack, docs/CLI.md + docs/NIMBUS.md, foreman-skill/spawn/
  conventions edits. Tracked by its own bead; lands as a handoff-style PR when Paul says.
- **Claimed, no live session (reconcile at next planning pass):** scratch-box provisioning
  `hm-f2s`/`hm-x9f` (P0, Paul's), CI benchmark `hm-w9s` (P1, Paul's), aarch64 public-api
  gate `hm-4aj`, PR #108 arrival-day validation `hm-f99`, AMD hammer dry-run `hm-8v4`.

## Ready (unblocked; foreman spawns as slots free — 2 of 3 slots in use; ~94 ready)

- **PR #147 park family remainder** (all P2): `hm-j7ie` (ledger VERSION 2→3 /
  refuse-vs-accept — decision-shaped; foreman drafts the doctrine-consistent spec next:
  bump + loud refuse, argued for Paul's veto at PR time), `hm-mmkf`, `hm-4gaw`,
  `hm-f82p`. Edge repairs 2026-07-23: `hm-wshf` now blocks on `hm-j7ie`; `hm-zwhi`
  upgraded to hard-block on `hm-x1ss`.
- **Box/toolchain reproducibility pair** (`hm-gfr1` static box definition; `hm-nji6`
  payload-pin reproducibility + certified-binary archive) — the aa3-recert-pins landmine
  turned into work items.
- **PR #138 follow-up family**: evidence-coverage family `hm-btht`, reseal genesis-rooted
  re-materialization `hm-kyy5` (truncation `hm-aqf0` is in flight as PR #147).
- **PR #146 deflake parks**: `hm-3r2k` (anchor phase-2 wait on `data: `), `hm-gfi2`
  (helper inline-or-generalize), `hm-gnxr` (doc amendment, P3).
- **Schedule-closure design question** (`hm-x1ss`): can the unsafe-snapshot-moment CLASS
  die at the root — now also owns the `hm-zwhi` fix direction.
- Large hardening backlog (PR #132/#134/#135 review-park families, AMD spike residue,
  quality/CI chores): ~100 open beads; see `bd ready` for the live order.

## Blocked (dependency edges enforce these — they surface via `bd ready` when cleared)

- **Software-system transfer chain**: ratify transfer `hm-zlx` (P1) ← planted-bug proof
  `hm-ebe` (P1) ← mechanism GO `hm-yjf` (a Paul decision, now READY — see above); then
  count-based Entry selector `hm-bfr` and the held-out SMB cooperative evaluation `hm-2su`.
- **AMD AE ladder box legs** (`hm-3gw` AE-5/AE-6, `hm-gig` AE-3 exact-landing) ← Epyc
  hardware (`hm-5wq` provider pick, `hm-f2s` scratch box).
- Dormant tier: live net-fault enforcement `hm-wvh`, triage suite `hm-4xe`, exact-pct
  `hm-6rv` (P3s behind strategy dispositions).

## Recently done (this week)

- **Advanced-span verdict folds MERGED** (tasks/152, PR #155, `hm-mmkf`, 2026-07-23
  evening — discovery (F1 CONFIRMED P1: fold-semantics change reached persisted
  checkpoints with no version boundary; judge stale-checkpoint repro) → one batch
  (ledger VERSION 3→4 loud refusal, satisfied-count `== 1` pin, fixture honesty) →
  verify clean (two-build repro: head refuses what pre-fix silently reopened; 4 P1
  claims at verify, 0 survived)): the PR #147 F4 false-absence class is closed —
  advanced-span occurrence/assertion events now judged. Parks: `hm-5mx0`
  (v1-verb test machine → e2e gate + StepReport surface), V1 folded into `hm-w1o6`.
- **SSE scanner hardening MERGED** (tasks/151, PR #154, `hm-b5km`+`hm-8c5m`,
  2026-07-23 evening — discovery (1 P1: stale test-count claim) → one batch → verify
  clean): scanner generic over io::Read with scripted-read coverage; phase-1 anchor
  fail-closed; ScriptedReader deleted for std Read::chain. The PR #146→149→152→154
  SSE-deflake arc is now fully closed with zero open parks.
- **Explorer contract clarifications MERGED** (tasks/150, PR #153,
  `hm-s6cb`+`hm-wshf`, 2026-07-23 late afternoon — full bounded pipeline: discovery
  (2 P1s: false genesis-coincidence doc claim; surviving mutant) → fix (re-key +
  judge-sanctioned inline killing the mutable target) → verify (new P1: the discovery
  record's own prescription was mis-keyed on parent_cut; superseded — true key is
  rollout.parent) → final doc-re-key batch → Closer re-check APPROVE): version refusal
  no longer misdiagnoses future versions; accessor contract honest for every
  production record shape. Park: `hm-0qpm` (observations_at up-to-translation
  qualifier, doc-only).
- **SSE bounded accumulating wait MERGED** (tasks/149, PR #152, `hm-38kv`, 2026-07-23
  afternoon — 5-seat tribunal, zero P1s, three P1 claims demoted/refuted with judge
  recomputation incl. an independent 500× stress): one structural scanner + bounded
  cumulative wait closes the PR #149 F1 wait-loop family; keepalive-skip coverage now
  synchronous. Parks: `hm-8c5m` (phase-1 header-terminator anchor), `hm-b5km`
  (genericize the scanner over io::Read + scripted-read tests; folds hm-8c5m).
- **Seal-capture reconciliation MERGED** (tasks/146, PR #150, `hm-whoo`, 2026-07-23
  mid-afternoon — full pipeline: 5-seat discovery REQUEST_CHANGES (F1 CONFIRMED P1:
  the worker's bounded-by-cut.at form admitted an appended future-Moment record;
  judge refuted the toy-faithfulness defense) → one fix batch (literal invariant,
  vtime-truncated toy read, clock-bounded BoxGuest fixture, structural prefix
  comparison) → verify event all-clear (every closure recomputed) → post-rebase
  contract-seat pass after the PR #151 conflict → merged): the seal-capture
  reconciliation is now count-complete AND content-aware (id/schema-swap refused).
  Parks: `hm-w1o6` (F3 interior-seal content validation + V-N2 diagnostic note) —
  now unblocked.
- **Ledger VERSION 2→3 MERGED** (tasks/148, PR #151, `hm-j7ie`, 2026-07-23 afternoon —
  5-seat tribunal, zero P1s, three seats zero findings): pre-144 suffix-only-seal
  ledgers refused loudly before any record decodes (V4 cross-version-identity class
  closed by refusal). **Foreman-encoded ruling (bump + loud refuse, no migration)
  stands flagged for Paul's veto** in the PR record. Parks: `hm-s6cb` (message
  misdiagnoses future versions). `hm-wshf` (accessor contract) unblocked.
- **Telemetry deflake follow-ups MERGED** (tasks/147, PR #149,
  `hm-3r2k`/`hm-gfi2`/`hm-gnxr`, 2026-07-23 afternoon — full 5-seat tribunal, zero P1s
  at discovery, merged with parks): `data: ` wait anchor (500× stress 0-fail), helper
  inlined (no second caller), doc trim. One P2 park `hm-38kv`: the new wait loop is
  unbounded/non-accumulating (hang-on-regression, split-marker hang, premature-exit
  flake — judge-reproduced) + zero positive keepalive-skip coverage; one bounded
  cumulative wait closes the family. Test-code only.
- **Seal run-forward suffix capture MERGED** (tasks/144, PR #147, `hm-aqf0`, 2026-07-23
  midday — full bounded pipeline: 5-seat discovery (F1 CONFIRMED P1: advanced-seal
  suffix invisible to descendant recomputation; judge-repro'd past two test maskings) →
  F1+F2 fix → verify (V1 CONFIRMED P1: F2 ride-along crossed cut frames, refused an
  honest production host) → V1+V2 fix (one-frame reconciliation, folds `hm-udgn`) →
  Closer re-check APPROVE): descendant recomputation now sees the advanced span;
  toy frame aligned to production catalog-inclusive stamps; C1 below-baseline hole
  appended to `hm-whoo`. `hm-udgn` closed with the merge.
- **Skills/hygiene/Nimbus residue MERGED** (tasks/145, PR #148, `hm-nsfl`, 2026-07-23
  midday, light tier): draft-fenced provider-neutral skill twins + Nimbus scratch-machine
  capability (docs/NIMBUS.md boundary: daemon owns credentials, repo owns none) +
  docs/CLI.md. The residue's ci commits + secret-hygiene stack verified ALREADY absorbed
  on main (PR #118 hosted runners, cargo-deny pin, hardened secret-scan.yml) — dropped,
  not lost. Item-(b) rename discrepancy recorded for Paul in the PR + IMPLEMENTATION
  record. `oob/gha-residue` + `ci/gha-migration` remain on origin as history.
- **Telemetry SSE-frame test deflaked MERGED** (tasks/143, PR #146, `hm-ftok`,
  2026-07-23 early): subscribe before announcing the SSE stream; three review parks
  filed (`hm-3r2k`, `hm-gfi2`, `hm-gnxr`).
- **MockBackend late-landing capability MERGED** (tasks/142, PR #145, `hm-40na`,
  2026-07-23 ~01:50, tribunal: zero surviving P1s): the genuine @3e7 shape is portably
  real for the first time; the load-bearing, judge-mutation-CONFIRMED finding — the
  PR #143 arm-site guard is INERT on the genuine late-landing shape (drain's
  crossed-marker clause refuses it post-step) — recorded on `hm-x1ss`; F2+F3 mock
  refinement parked as `hm-j16h`.
- **pvclock arm-seam fail-closed guard MERGED** (tasks/140, PR #143, 2026-07-23 early —
  full tribunal pipeline: 5-seat discovery → F2+F3+F4 batch → verify (V1 protocol-v9
  bump, V2 Miri split, V3 strategy arm) → Closer re-check): silent staged-Moment
  overshoot on pvclock guests now refuses loudly at the arm site
  (`ScheduleMomentUnreachable`, wire v9); **`hm-zwhi` stays OPEN** — the @3e7 cure is
  the `hm-x1ss` decision; box discrimination runbook shipped; hm-40na filed and
  dispatched.
- **AA-6 injection attestation MERGED** (tasks/141, PR #144, `hm-oh3v`, 2026-07-22 late):
  run-set injection stamp + per-record fired witness; `check_aa6_matrix` fail-closed on
  missing/OFF/zero-fired/incoherent; three planted-failure fixtures RED-asserted.
- **AA-6 masked-register-digest apparatus MERGED** (tasks/138, PR #142, 2026-07-22 late
  eve): closed-list {x29, SP} mask pinned to the on-N1 register dump, fail-closed lane
  checker with negative controls, `injected_landed_digest` witness (`hm-fiqo` closed),
  turnkey runbook; AA-6 headline honestly reads PROVISIONAL GO now. On-silicon leg =
  the parked `hm-3bwm` lane.
- **Mutants-runner survivability MERGED** (tasks/139, PR #141, `hm-y53x`, 2026-07-22 late
  eve): preemption diagnosis with contrast-run evidence, `timeout-minutes: 90`, infra-only
  retry (never retries a real red); live-proven on the PR's own CI. Costed options + the
  migration un-sharding flag → §Decisions.
- **AA-3 on-silicon re-cert GO recorded** (PR #140, 2026-07-22): regenerated-pin basis,
  Paul-ruled; 12 green ≥10⁶ campaigns; evidence archived on `task/arm-aa3-recert`
  (origin branch + tag `archive/aa3-recert-evidence`). `hm-idb` (the whole ARM spike
  execution) CLOSED.
- **AA-6 Linux-guest injection matrix — PROVISIONAL GO** (tasks/135, PR #139, `hm-zx3z`,
  2026-07-22): all four gate-semantics changes Paul-ratified (Fable second-opinion);
  full-GO upgrade rides `hm-3bwm` + `hm-l1wy`.
- **First cooperative Differential exploration gate — the maze vertical** (tasks/134,
  PR #137, `hm-qcpp`/`hm-cs5`, 2026-07-22): M2 @1e7 accepted per Paul's Option (a);
  the @3e7 pvclock overshoot filed as `hm-zwhi`, not silently folded.
- **Marker-clamped run-forward for candidate-seal quiescence** (tasks/136, PR #138,
  `hm-esfd`, Option C, 2026-07-22); its J1/J2/J5 verify findings parked as
  `hm-kyy5`/`hm-btht`/`hm-aqf0`.
- **ARM AA-5(c) guest-Linux substrate + AA-4 W^X apparatus** (PR #135, `hm-9r1`+`hm-rfz`,
  2026-07-21); hardening families parked as `hm-of6t`/`hm-l1wy`/`hm-7o68` + F1–F5 beads.
- **SelectorV1 exploit path — seal-consistent frontier env** (tasks/133, PR #136,
  `hm-0paj`, 2026-07-21).
- **Full Differential vertical** (tasks/132→PR #134, `hm-e6q`, 2026-07-21): production DD
  relations + SMB through the two-barrier controller + legacy-spine retirement; mutants CI
  sharded 4-way; large verify-park family (V9–V11, F4–F9) filed as beads.
- **harmony-linux guest environment tier** (PR #133, 2026-07-21).
- **ARM/Altra vendor determinism spike** (PR #132, 2026-07-20): AA-3 physics + AA-4
  ruling + AA-5(a/b); J-series parks filed; the later account-wipe → the pins landmine
  (`bd memories aa3-recert-pins-landmine`) → `hm-gfr1`/`hm-nji6`.
- **Campaign evidence retention policy** (tasks/131, PR #131, `hm-5sv`, 2026-07-17) and
  **Explorer↔Differential cells+archive integration** (tasks/130, PR #130, `hm-bbx.4`,
  2026-07-17) — the Differential migration epic's last structural children.
