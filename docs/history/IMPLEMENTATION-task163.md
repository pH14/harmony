<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Task 163 (W5, hm-nsev) — docs and labels match committed evidence

Pure documentation pass, no code/gate/evidence-file changes (verified: `cargo build --workspace`
and `cargo fmt --check` both green, and `git diff --stat` touches only `docs/`, `spikes/*/`, and
`tasks/*.md`). Closes `hm-7pm`, `hm-d8g`, `hm-472`, `hm-4o4`, `hm-ixys`.

## The review table

| Bead | Claim | Prose said | Evidence shows | Artifact |
|---|---|---|---|---|
| hm-7pm | AA-5(c) register identity | "architectural (console + register) determinism" (2 sites) and "console + register digests held bit-identical" (1 site) in `docs/ARM-ALTRA.md` | Register digest was measured on a **separate nokaslr diag build**, not the shipped/pinned image, on 2026-07-20. The 2026-07-21 F12 re-cert measured `regs_digest`/`core_regs_digest` **on the pinned image itself** and found it diverges same-seed — 4/260 registers (`x29`/`SP`, entropy-derived stack placement, not KASLR) | `spikes/arm-altra/results/aa-5/live-20260721/README.md` |
| hm-d8g | AE-4 "MSR default-deny" demo | `spikes/amd-epyc/contract/enforcement-truth-table.md`:58 and `spikes/amd-epyc/IMPLEMENTATION.md`:65 called the demo "MSR default-deny" | `ae4-msr.c` installs `KVM_MSR_FILTER_DEFAULT_ALLOW` denying only one MSR (HWCR); `deny_gp_then_shutdown=0` — the demo proves the trap mechanism, not a default-deny posture. A real default-deny demo (filter `DEFAULT_DENY`, an unlisted-MSR read, the injected-`#GP` shutdown path exercised) is deferred to W6's AMD box window | `spikes/amd-epyc/harness/src/ae4-msr.c:29-46`, `spikes/amd-epyc/results/ae-4/msr-deny.json` |
| hm-472 | AA-2 step-replay coverage | `docs/ARM-ALTRA.md`'s AA-2 disposition: "replay-identity PASS — 85,165 stepped groups each bit-identical across reps" with no coverage-boundary caveat | Every step but a group's last carries only `regs_digest` (registers, no RAM); only the final step of each group carries the full-payload `state_digest`. Register-level replay-identity holds at every intermediate step; full-memory replay-identity is established at each group's final step, not continuously | `spikes/arm-altra/harness/src/run.rs` (`regs_digest` call ~L1786, final-step `state_digest` stamp ~L1950) |
| hm-4o4 | `docs/LAYERS.md` vs the 7 gh#77-named pending task specs | gh#77 (2026-07-06) asked for reconciliation edits across tasks 43/66/70/71/80/81/82/83/94 | Tasks 43, 66, 71, 80, 81, 82 are **done** (crate + `IMPLEMENTATION.md` exists, or task 43's own R-L4 amendment note) — historical record, not churned. Task 83 is pending but already vocab-clean. Task 70 is pending: fixed stale "Progression" → "search loop" (glossary + shipped code), and did **not** add gh#77's requested R-L2/task-84-on-ramp content — both superseded by LAYERS.md's own 2026-07-12 amendments. Task 94 flagged (not resolved) as likely superseded by the glossary's rollout/step naming, per gh#77's explicit "don't silently supersede" instruction | `docs/LAYERS.md` R-L1/R-L2 amendments, `dissonance/explorer/src/spine.rs:39,96`, `dissonance/{matcher,tactics-regime,resolution}/IMPLEMENTATION.md`, `tasks/84-exploration-gate.md`'s banner |
| hm-ixys F13 | AA-6 MANIFEST "DEMONSTRATED" wording | Bug asked to tighten wording so a FAIL-containing run isn't read as green-on-fail | Already fixed by a prior bead (hm-7q0, dated 2026-07-25 in the file) — the MANIFEST already has a machine-checkable scoped floor-check verdict and explicit "no prose reconciliation of a full-run FAIL" language | `spikes/arm-altra/results/aa-6/live-20260720/MANIFEST.txt` |
| hm-ixys F14 | AA-5(c) disposition ladder verdict | No one-word GO/PROVISIONAL verdict on the AA-5 disposition, unlike AA-1/AA-6 | Added explicit **PROVISIONAL GO** (evidence clean but bounded: full-RAM identity open behind the entropy residual), matching the doc's own ladder convention | `docs/ARM-ALTRA.md` AA-5 disposition |
| hm-ixys F15 | exit-43 forward coordination | No note that `KVM_CAP_ARM_STAGE2_EXEC_GUARD`'s exit 43 collides in number (not in code) with the unrelated 0005 `KVM_EXIT_DET_STEP` patch's exit 43 | Added a forward-coordination note: different box kernels today, no shared UAPI, but a future host carrying both patch stacks needs one renumbered | `docs/ARM-ALTRA.md` AA-4 Level-3, `docs/CPU-MSR-CONTRACT.md`, `docs/R-BACKEND.md` |
| hm-ixys F18 | `build-window-hosts.sh:37` stale path | Bug reported a `~/harmony/guest/dl` fallback path broken by the harmony-linux rename | Already fixed — the script already reads `~/harmony/harmony-linux/dl`; no stale path found | `spikes/arm-altra/host/build-window-hosts.sh:37` |
| hm-ixys F4 | `machine.rs:1368` `copy_nonoverlapping` soundness | Bug asked to confirm the SAFETY comment is accurate (refuted finding, non-blocking) | Confirmed sound as-is: two distinct, uniquely-owned, same-length mmaps; SAFETY comment already states this precisely. No change | `spikes/arm-altra/harness/src/sys/machine.rs:1365-1368` |

## Files touched

- `docs/ARM-ALTRA.md` — hm-7pm (3 sites), hm-472 (1 site), hm-ixys F14 + F15
- `spikes/amd-epyc/contract/enforcement-truth-table.md`, `spikes/amd-epyc/IMPLEMENTATION.md` — hm-d8g
- `tasks/70-selector-bandit.md`, `tasks/94-rename-modulation-progression.md` — hm-4o4

## What was deliberately NOT touched

- `spikes/arm-altra/harness/src/run.rs` / `sys.rs`'s own "caught end-to-end" doc-comments (hm-472)
  — code, out of this task's scope; the prose fix landed in `docs/ARM-ALTRA.md` instead.
- Per-step memory commitments / dirty-page tracking (hm-472's suggested follow-up) — a real
  mechanism change, not a prose fix; left for a future bead if ever prioritized.
- `docs/RESOLUTION.md`'s own stale `Environment`/`Reproducer` naming, surfaced while auditing
  hm-4o4 — out of scope (hm-4o4 is LAYERS.md vs the named task specs, not a RESOLUTION.md pass),
  and resolving it requires judgment about which of two distinct renamed/unrenamed identifiers
  (`explorer::Reproducer` vs `environment::Environment`) each site means; not fixed here to avoid
  inventing an incorrect correction.
- `docs/QUEUE.md` — explicitly out of scope per the task spec (foreman regenerates it).
- No new beads were needed: every code-shaped observation surfaced during the audit (F18, F4) was
  already resolved in the tree, so nothing here reveals a live, unfiled code defect.

## Gates

`cargo build --workspace` and `cargo fmt --check` both pass (tripwire only — no code moved). No
doc touched here is machine-parsed by a test or checker (checked via `git grep` for path
references into `.rs`/`.py`/`.sh`; all hits are comment provenance links, not parsers).
