# Task 163 — W5: docs and labels match committed evidence (hm-nsev)

**Work order:** `hm-nsev` (P1 epic, label `work-order`). Read it first — `bd show hm-nsev`
— then every child with `bd show <id>`. **This is a documentation-correction task. Do not
change code, gates, or evidence** — when prose and evidence disagree, the evidence wins and
the prose moves.

## Why this is one batch and not five fixes

The failure mode is systemic: prose drifting from evidence in **both directions**. A single
pass with both directions in view catches what five separate fixes would not — because the
instinct when you find an overclaim is to soften everything, and the instinct when you find
an underclaim is to firm everything up. Doing both at once forces you to ask, each time,
*what does the committed evidence actually support?* rather than which way to lean.

That question is live right now. Two reviews merged this morning turned on exactly it: a
grader printing "the manifest records the full matrix" while checking nothing of the kind
(PR #160), and an implementation record quoting a benchmark delta with its dilution caveat
stripped (PR #161). Same species. You are cleaning up the standing instances.

## The children

### Underclaim — safe direction, still wrong

- **`hm-7pm` (PR135 F1)** — `docs/ARM-ALTRA.md` says the execute-guard is
  blocked-on-live-proof and that the AA-5(c) path has not run on the Altra. **Both are
  contradicted by results committed in the same PR.** Find those results, cite them by path,
  and correct the prose to what they show. An underclaim is not harmless: it makes the
  project look less proven than it is, and the next person re-runs work that was already
  done.

### Overclaim — the dangerous direction

- **`hm-d8g` (PR128 P2-3)** — AE-4 prose calls a **default-ALLOW** demo "MSR default-deny".
  Relabel it to what the demo does. The box-side half (a real default-deny demonstration)
  belongs to work order W6's AMD window — record precisely what that run must show, and do
  not imply it has happened.
- **`hm-472` (PR132 J3)** — `docs/ARM-ALTRA.md` says step replay is "caught end-to-end" when
  intermediate steps are **register-only and memory-blind**. State the actual coverage
  boundary. If closing the gap needs work, that is a bead, not a prose fix.

### Consistency

- **`hm-4o4`** — reconcile `docs/LAYERS.md` against the pending task specs (gh #77).
- **`hm-ixys`** — the PR135 P3 wording nits (F13 DEMONSTRATED wording, F14 ladder verdict,
  F15 forward-coordination, and the rest — read the bead for the full list).

## Method — non-negotiable

For **every** claim you touch: find the committed artifact that settles it (a results file,
a manifest, a log, a test), and cite it by path in the commit message. If you cannot find an
artifact that settles a claim, **say so and leave the claim alone with a note** — inventing a
correction is worse than the drift you were sent to fix. A prose change with no cited
evidence is not this task's deliverable.

Where a claim is *partly* true, say exactly which part. "The kernel-dependency half is
proven; the enforcement leg did not exercise" is the shape to aim for — that phrasing came
out of this morning's `hm-rdp` review and it is the house style for a bounded claim.

## Scope boundaries

- **No code, no gate, no evidence-file changes.** If a doc fix reveals a real code defect,
  file a bead and keep going.
- Do not touch `docs/QUEUE.md` — the foreman regenerates it.
- If you find an overclaim **not** on the children list, fix it in the same pass and say so
  in the PR body; that is squarely within this work order's purpose.

## Gates

`cargo fmt --check` and the workspace build must still pass (you should not have moved any
code, so this is a tripwire, not a real gate). If any doc you edit is referenced by a test
or a checker — some manifests are machine-read — run that checker and say which.

## Deliverable

PR from `task/docs-match-evidence` closing `hm-7pm`, `hm-d8g`, `hm-472`, `hm-4o4`, `hm-ixys`
with the merge. The PR body should be a table: claim → what the prose said → what the
evidence shows → the artifact path. That table is the review.
