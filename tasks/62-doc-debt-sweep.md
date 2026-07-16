# Task 62 — doc-debt sweep: make the docs stop lying to newcomers (and the foreman)

> **DELEGABLE (mac-local, docs + skills only) · one integrator ruling required.** The 2026-07
> review (`docs/REVIEW-2026-07.md` gap #7) found the strategy docs a full wave stale and in
> places self-contradictory. This is a single sweep PR fixing all of it. Every item below names
> its exact target; the two items marked **[RULING]** need one-line decisions from the
> integrator — collect them in the PR description before merging, do not guess.
>
> Land when the docs-touching queue is clear (this conflicts with anything editing
> `docs/ROADMAP.md`/`docs/PLAN.md`). No code, no goldens, no hashed inputs (see D below — explicitly
> out of scope here).

## A — the strategy docs

1. **Rewrite `docs/ROADMAP.md`** as one honest current page: Wave-3 outcome (closed), the 47–57
   V-time/preemption/SMP arc (merged/box-verified), Wave 4 = tasks 58–62/93/94 per
   `docs/REVIEW-2026-07.md`, and the deferred register (D1, D5, SDK/coverage epoch, ARM, task
   92, task 43). Delete the stale "Sequenced backlog" table (history lives in git).
2. **Split `docs/PLAN.md`**: it is the citation anchor for the CPU/MSR contract
   (`docs/CPU-MSR-CONTRACT.md:441,1481` cite it) so it cannot die — restructure into
   (a) "Frozen determinism axioms" (the trap table, V-time-from-retired-branches, the
   nondeterminism-source table — still true, still citable) and (b) "Original plan (historical;
   superseded — see ROADMAP)" containing the rest. Fix in place while splitting: bare `restore`
   (`docs/PLAN.md:31` — contradicts DISSONANCE.md's no-bare-restore rule), "VMCALL" (the mechanism is
   a port-I/O doorbell), `ext-net` + the fault-injecting bridge (retired pv-net model),
   "3-node etcd" (superseded by single-node Postgres/k3s).
3. **`docs/DETERMINISM-CORPUS.md`**: tasks 22/23 are struck (`docs/ROADMAP.md` already says so)
   — retarget the C3a entry to Postgres-on-RAM, fix the device-surface table
   (`DETERMINISM-CORPUS.md:96-120,168-182`).
4. **`docs/DISSONANCE.md`**: add `dissonance/flow` to the crate registry (built by task 51,
   absent from the table at `:302-307`, and described at `:289` as unbuilt — false); reconcile
   the quiescent-only-snapshot language (`:174`) with task 41's merged non-quiescent capture;
   note that `perturb` enforcement is task 59 and the net vertical is task 61.
5. **`IMPLEMENTATION.md` name collision**: `docs/IMPLEMENTATION.md` is task 06's private review
   diary, root `IMPLEMENTATION.md` is quality-a's — rename both to
   `docs/history/IMPLEMENTATION-task06.md` / `docs/history/IMPLEMENTATION-quality-a.md`
   (with `docs/IMPLEMENTATION-quality-d.md` joining them) and fix inbound references. The
   authoritative-looking names should not be squatted by per-task diaries.
6. **Phantom task 91**: `tasks/16-patched-kvm-rdtsc-spike.md:3` references a task 91 that was
   never written. Reword to point at what exists (`consonance/vmm-backend/kvm-patches/` +
   task 57).

## B — the constitution and the skills

7. **Amend `tasks/00-CONVENTIONS.md`** with a **frontier-task class**: box-only, may touch
   multiple named crates (the spec's surface list is the boundary instead of rule 1), gates are
   box gates + portable-logic gates, and specs must remain runnable-from-the-repo (box paths and
   tags belong in the spec's Environment section, not scattered as prose). ~2/3 of tasks 41–57
   were already this class; the document every worker reads first should describe it.
8. **Skill consistency**: `1884b9a` dropped the pi (GPT-5.5) second reviewer from `pr-review`,
   but `.claude/skills/handoff/SKILL.md:15-16,84-85` still mandates "codex + pi" and
   `.claude/skills/foreman/SKILL.md:163,180` still treats `pi auth` as live. Make all three
   agree (codex is the cross-model pass).

## C — the ruling items **[RULING]**

9. **The single-vCPU ruling.** "One vCPU, period" is load-bearing in the contract topology
   (`docs/CPU-MSR-CONTRACT.md:441`), DISSONANCE.md's one-outstanding-decision model (`:224`),
   and `Moment = InsnCount` — and task 56 shipped a `CONFIG_SMP=y` guest (`maxcpus=1`) without
   any doc acknowledging it. Record the integrator's ruling in DISSONANCE.md + a contract
   margin note. Recommended shape: *v1 contract = SMP-built kernel, exactly one **online**
   vCPU; real multi-vCPU is out of scope until explicitly re-ruled* — deferred, not foreclosed
   (deterministic SMP is a potential edge over Antithesis; see REVIEW-2026-07).
10. **Task 90 close-out posture.** Task 90 is ~95% executed but marked fully pending; its real
    stragglers are `hypervizor` strings in **hashed build inputs**
    (`guest/linux/lib-build.sh:48,59,60` — the task-43 landmine: changing them invalidates
    `MANIFEST.sha256`). Ruling: document-as-deliberately-stale (recommended; a comment at each
    site + a task-90 close-out note) or schedule a rebaseline PR. Update `tasks/90-*.md` to
    reflect what is actually done either way. **Do not touch the strings in this task.**

## Acceptance gates

1. Every numbered item done or its **[RULING]** recorded verbatim in the PR + target doc.
2. `git grep -n 'pv-net\|VMCALL\|task 22\|task 91'` over `docs/` returns only
   deliberately-historical references (each with an inline historical note).
3. A newcomer path-test, stated in the PR description: reading README → ROADMAP →
   REVIEW-2026-07 → DISSONANCE.md yields the true current state (name what each doc now claims
   the frontier is).
4. No code, golden, or hashed-input changes (`git diff --stat` shows docs/, tasks/, .claude/
   only).

## Non-goals

Executing task 90's remaining renames or any rebaseline; the task-43 move; ROADMAP entries for
work not yet ruled (SDK epoch details live in REVIEW-2026-07's deferred register, not ROADMAP).
