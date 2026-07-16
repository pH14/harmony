# Task 102 — Nested-x86 re-certification: fix the harnesses, re-run the evidence

Ruled by Paul 2026-07-12 (top priority): the nested-x86 spike's ALL-GO dispositions are not
supported by the retained evidence (PR #98 round-1 review, comment 4951191686 — 12 P1s, 4
foreman-verified). **No behavioral failure was found** — the failures are of proof. This task
re-earns the certification. Work happens on the existing `spike/nested-x86` branch (PR #98
stays open; it merges only when dispositions match evidence).

Program record: `docs/NESTED-X86.md` (binding — acceptance criteria and box discipline);
the review comment enumerates every finding. Beads: `hm-recert-*` chain (see `bd show`).

## M1 — harness integrity fixes (Mac-local + code review; NO box needed)

Fix, on the spike branch:

1. **Gate-RC propagation**: `run-appliance.sh` (and every N-2/N-3 caller) must fail on any
   gate rc != 0 — the `NESTED_X86_L1_DONE` marker is never a success condition. The N-5
   script's explicit `GATE_RC ... rc=0` greps are the pattern.
2. **N-2 hammer targets the patched mechanism**: construct `PatchedKvmBackend` (verify
   `arm_preempt_exit`/MTF actually armed — assert on the capability, don't assume), fix the
   RAM-drop-before-backend order (declare memory first), and make the new unsafe path
   Miri-reachable or factor the duplicated allocation logic away.
3. **Overflow accounting**: count perf records/exits per armed deadline; the claim "zero
   missed/duplicate overflows" must be measured per-record, not inferred from a final total.
4. **Independent guest oracle**: payloads expose an analytically-known, guest-side progress
   count (memory-visible, hashed) so count-exactness is judged against an oracle on a
   different axis than the PMU itself.
5. **Hash verification before boot** (not just recording) for every artifact, and **commit
   the build manifest** for the appliance image the runs actually test (the current
   accepted evidence cites an image with no committed manifest — provenance must close).
6. **Pause harness**: parameterize the cadence, record it in evidence, default to the
   accepted (2s/30s) cadence; count only QMP cycles that succeeded (check responses; no
   `|| true` swallowing).
7. **Audit the retained runsets** against raw console logs; annotate every runset whose
   green flowed through the marker-only check (runset-001 is the known demonstrator).

Gate: foreman review of the branch diff + a clean cross-model pass on the harness changes.

## M2 — re-run N-2 with the fixed instrument (box; AFTER the game-workload box window)

The full N-2 matrix from docs/NESTED-X86.md — count exactness, overflow delivery, exact
landing, contamination probes — ≥1,000,000 armed deadlines cumulative, through the patched
backend, with per-record accounting and the independent oracle. Provisional-GO threshold and
NO-GO conditions verbatim from the doc. Smoke-fire-once before the full spend.

## M3 — re-run N-3 at its floors + re-certify (box)

≥1,000 same-seed full-gate repetitions per condition (solo / co-tenant stress / vCPU
migration / pause-resume), every sample accounted for. **Parallelization across distinct
pinned cores is permitted and encouraged** per the task-69 M2 standing directive — co-running
is itself a determinism stress test; solo==co-tenant `state_hash` must hold; divergence is a
P0 STOP+escalate, never serialize-to-hide. Then: re-record every disposition in
docs/NESTED-X86.md from the new evidence (GO only where the new evidence meets the doc's own
criteria — a criterion revision, if ever needed, is a Paul ruling, never silent), refresh the
N-5 demo, and hand PR #98 back to the foreman for the merge read.

## Box discipline (binding, from the doc + memories)

Reachability test first (`ssh hetzner true`), record-then-modify with a restore manifest,
restore + verify at every yield, content-hash-verify every boot artifact, taskset-pin every
run, separate write/launch ssh calls with `</dev/null` (pkill/pgrep argv landmine), and the
L0 nested-posture flip must not run concurrently with another workload's box window —
coordinate via the foreman (the game workload's M0 window has precedence today).

Close the `hm-recert` beads per milestone; the foreman closes the final one on PR #98 merge.
