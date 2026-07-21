# IMPLEMENTATION — task 62 (doc-debt sweep)

Docs/specs/skills only, per the task's non-goals (no code, golden, or hashed-input changes with
one narrow, explicitly-directed exception — see "Deviations" below).

## What landed

**A — strategy docs**
1. `docs/ROADMAP.md` rewritten as a current-state page: Wave-3 outcome (closed), the 47–57
   merged/box-verified arc, Wave-4 table (58–62/93/94), the deferred register (D1/D5, SDK
   epoch, ARM, task 92, task 43), and both ruling sections. The old "Sequenced backlog" table is
   gone (history lives in `git log -- docs/ROADMAP.md`).
2. `docs/PLAN.md` split into **Part A** (frozen determinism axioms: platform, V-time-from-retired-
   branches, the trap table, gate discipline — still citable, unchanged citation anchors for
   `docs/CPU-MSR-CONTRACT.md:441,1481`) and **Part B** (original plan, marked historical). Fixed
   in place while splitting: bare `restore` in the control-API sketch (now annotated — the real
   transport has no bare restore, only `replay`/`branch`), "VMCALL" (annotated — the real
   mechanism is the port-I/O doorbell), `ext-net` + the fault-injecting bridge (annotated
   retired — replaced by the per-flow, host-decides/guest-enforces model), "3-node etcd" (
   annotated superseded — Postgres/k3s is the actual Wave-3 choice).
3. `docs/DETERMINISM-CORPUS.md`: tasks 22/23 struck throughout (C3 section, device-surface
   table, sequenced backlog, recommended order, non-goals); C3a retargeted to Postgres-on-RAM
   (tasks 36–38/48/49, already delivered) in place of the SQLite-over-`Block` design, which is
   kept as a historical record only. `docs/BLOCK-DEVICE.md` marked historical (it grounds the
   now-struck tasks 22/23).
4. `docs/DISSONANCE.md`: added `dissonance/flow` to the crate table (task 51, built, previously
   absent — the table said "unbuilt" for the exact thing task 51 shipped); reconciled the
   quiescent-only-snapshot language (task 41 lifted that limit — snapshots now work at any
   V-time point, not just quiescent ones); noted `perturb` enforcement = task 59, net vertical
   wiring = task 61 in the "What is still open" section.
5. `IMPLEMENTATION.md` name collision fixed: `docs/IMPLEMENTATION.md` (task 06's diary) →
   `docs/history/IMPLEMENTATION-task06.md`; root `IMPLEMENTATION.md` (quality-a's diary) →
   `docs/history/IMPLEMENTATION-quality-a.md`; `docs/IMPLEMENTATION-quality-d.md` →
   `docs/history/IMPLEMENTATION-quality-d.md`. Fixed the one inbound reference
   (`docs/CPU-MSR-CONTRACT.md`'s `[question]` pointer). This file joins them as
   `docs/history/IMPLEMENTATION-task62.md`.
6. Phantom task 91 fixed: `tasks/16-patched-kvm-rdtsc-spike.md:3` no longer references the
   nonexistent task 91; reworded to point at `consonance/vmm-backend/kvm-patches/` + task 57
   (the canonical kernel port that folded the patch series in).

**B — constitution and skills**
7. `tasks/00-CONVENTIONS.md` amended with a **frontier-task class**: box-only, spec-named
   surface list instead of hard rule 1's single-crate scope, box gates + portable-logic gates,
   and a requirement that frontier specs keep box paths/tags in an Environment section rather
   than scattered prose.
8. Skill consistency: `.claude/skills/handoff/SKILL.md` and `.claude/skills/foreman/SKILL.md`
   updated to say codex (GPT-5.5) is the sole cross-model pass, matching `pr-review/SKILL.md`
   (already correct post-`1884b9a`). Removed the remaining "pi" mentions (mandate text, `pi auth`
   ground rules).

**C — ruling items** (integrator rulings supplied directly by the user, not re-derived)
9. **Single-vCPU ruling.** *v1 contract = an SMP-built kernel with exactly one **online** vCPU;
   real multi-vCPU is out of scope until explicitly re-ruled — deferred, not foreclosed.*
   Recorded in `docs/DISSONANCE.md` (new "Ruling" section right after Naming) and as a margin
   note at the top of `docs/CPU-MSR-CONTRACT.md` §2 (the frozen-topology section feeding the
   `:441`/`:1481` citations), plus a one-line pointer in `docs/ROADMAP.md`.
10. **Task 90 close-out.** *Document-as-deliberately-stale.* A close-out note at the top of
    `tasks/90-rename-harmony.md` records the ~95%-done state and the ruling; comments at
    `harmony-linux/linux/lib-build.sh:48,59,60` point back to it. **The `hypervizor` strings themselves
    are untouched.**

## Newcomer path-test (acceptance gate 3)

README → `docs/ROADMAP.md` → `docs/REVIEW-2026-07.md` → `docs/DISSONANCE.md`, read in order,
now yields a consistent frontier claim at each stop:

- **README** — one-paragraph pitch, no stale claims to begin with (unedited).
- **ROADMAP** — Wave 3 is closed (full stack height, Postgres→k3s, deterministic-twice); the
  47–57 arc is merged; **Wave 4 (58–62/93/94) is the current frontier**, with 58/59/60/61 not
  yet started, 62 = this document, 93 resolved ahead of 58, 94 queued after 58–61. Both rulings
  (single-vCPU, task-90) are stated here and cross-linked to their normative homes.
- **REVIEW-2026-07** — unchanged (it's the review that produced this sweep's task list; its
  claims about Wave-3 outcomes and the ranked gaps match what ROADMAP now says, since ROADMAP
  was rewritten *from* it). A reader lands on the same "the product loop has never run once"
  framing ROADMAP's Wave-4 section now states directly.
- **DISSONANCE.md** — the two-plane/two-loop design, now with `dissonance/flow` correctly
  listed as built (task 51), the quiescent-only limit correctly described as lifted (task 41),
  and the single-vCPU ruling stated at the top rather than left implicit in `Moment = InsnCount`.

All four docs now agree: **Wave 3 (workloads) is done, Wave 4 (closing the dissonance↔consonance
loop) is the frontier, and two axioms that were silently cracked (single-vCPU, task-90 rename
completeness) are explicitly ruled rather than ambiguous.**

## Deviations considered and rejected

- **Leaving `harmony-linux/linux/lib-build.sh` untouched, gate 4 taken literally.** The task spec's
  acceptance gate 4 says `git diff --stat` should show `docs/`, `tasks/`, `.claude/` only. The
  user's follow-up message (which supplied both rulings) explicitly directed comments at
  `harmony-linux/linux/lib-build.sh:48,59,60` as part of ruling C10's deliverable. Comment-only,
  no hashed string touched (`hypervizor` is unchanged byte-for-byte) — this is the one
  intentional exception to gate 4, made on explicit instruction rather than by inference.
  Flagging it here rather than silently expanding scope.
- **Not touching `docs/CPU-MSR-CONTRACT.md`'s or `docs/INTEGRATION.md`'s many VMCALL rows.**
  Gate 2 greps for `VMCALL` across `docs/`; most hits there are the *normative contract's*
  accurate technical description of the VMCALL instruction's disposition (already correctly
  distinguished from the actual port-I/O-doorbell transport, inline, with citations). These are
  current content, not doc debt — editing them risked breaking a hashed-adjacent normative
  document for no correctness gain. Fixed instead: `docs/BLOCK-DEVICE.md` (no prior historical
  marker) and `docs/DETERMINISM-CORPUS.md`'s one loose "VMCALL channel" mention.
- **Not rewriting task specs 01/07/10/14/15/18/20/24/26/35/41/43/48–51/92** that mention
  `VMCALL`/`pv-net`/task 22 in their own historical/merged context. These are per-task specs,
  many already merged and historically accurate for the state at merge time (e.g. task 10's spec
  correctly describes the ABI *as it was speced*, later superseded by task 20 — that supersession
  is itself documented in task 20's own spec). Out of scope for a doc-debt sweep whose named
  targets (per the task file) are the six items in section A plus 00-CONVENTIONS/skills.
- **Not touching `deny.toml:31`'s "task 91" citation or `.github/workflows/quality.yml:61`'s
  same citation.** Both are non-doc files (out of the docs/tasks/.claude surface); the task
  spec's item 6 names only `tasks/16-patched-kvm-rdtsc-spike.md:3`. Flagging here in case a
  follow-up wants them fixed too — same phantom-task-91 root cause.

## Known limitations

- `docs/PLAN.md` Part B (historical) still contains the original architecture ASCII diagram and
  phase list verbatim except for inline annotations at the four specifically-named contradiction
  points (rule 2's list). It was not fully rewritten — task 62 asked for fixes "in place," not a
  full historical rewrite.
- The "deliberately-historical inline note" bar (gate 2) was applied narrowly: files with
  already-accurate technical VMCALL content were left alone; only files making stale claims
  (BLOCK-DEVICE.md, DETERMINISM-CORPUS.md, docs/PLAN.md) were annotated. If a stricter reading of
  gate 2 is wanted (every VMCALL mention gets an inline note regardless of accuracy), that's a
  larger follow-up sweep across `docs/CPU-MSR-CONTRACT.md`/`docs/INTEGRATION.md`.

## For the integrator

Both rulings (single-vCPU, task-90) came from the user directly in this session, not re-derived
or guessed — see `docs/DISSONANCE.md`'s ruling section and `tasks/90-rename-harmony.md`'s
close-out note for the verbatim text.
