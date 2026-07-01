---
name: handoff
description: >
  Prepare out-of-band work (done on a side branch, outside the foreman's task queue) for
  the foreman loop to pick up: put it on a well-named branch, open a DRAFT PR with a
  review-grounding description, associate it to a future task or a documented decision
  point, and mark it ready when complete. Invoke this in a side-branch session when you've
  built something the foreman should then review, iterate, and merge.
---

# Handoff — hand out-of-band work to the foreman loop

The foreman loop normally drives `tasks/NN-*.md` specs end to end. This skill is for work
done **outside** that queue — a design ruling, a tooling spike, a doc — that you still want
to flow through the **same** machinery: the mandatory cross-model review (`codex review`,
GPT-5.5 — the sole cross-model pass since `c21711d` dropped pi), the iterate-until-clean cycle,
and auto-merge. The foreman picks
up any **ready (non-draft) PR**; this skill makes sure the PR is shaped so that pickup works
and the review is grounded.

Do these in order. Stop and ask the user if a step is ambiguous (e.g. which task number).

## 1. Land the work on a clearly-named, non-`task/` branch

The foreman treats `task/<slug>` branches as its own (it spawned/will spawn a worker for
them). Out-of-band work must **not** use that prefix. Use a descriptive prefix instead:

- `docs/<topic>` for docs/decisions (e.g. `docs/r1-device-model-ruling`)
- `spike/<topic>` for an out-of-band spike
- `oob/<topic>` for anything else

If your work isn't already on such a branch, create one now (don't commit to `main`).

## 2. Associate it — future task OR documented decision point

Every handoff must say what it *is* in the project's terms, so the review knows what
"correct" means and the foreman knows what it unblocks. Pick one:

- **Documented decision point** (a ruling/design choice — e.g. the R-series rulings in
  `docs/ROADMAP.md`): add the ruling as `docs/<NAME>.md`, update `docs/ROADMAP.md` to mark
  it resolved, and correct any now-refuted lines in `docs/INTEGRATION.md`. State the
  decision, the evidence (cite sources — don't assert from memory), the constraints it
  imposes, and what it unblocks.
- **Future task**: if this is implementation that will become a tracked task, add a brief
  `tasks/NN-<slug>.md` spec (next free number; follow the shape of existing specs — Public
  API as a contract, acceptance gates, determinism notes) and reference it from the PR. The
  foreman can then track it like any task.

If it's neither yet, write the PR description thoroughly enough to stand in as the spec
(see step 4). `AGENTS.md` already carries the project-wide review bar, so the description
only needs the work-specific intent.

## 3. Open the PR as a DRAFT

Draft is the "still mine, don't touch yet" signal — the foreman skips draft PRs entirely,
so it won't review-on-every-push while you iterate.

```sh
gh pr create --draft --base main --head <your-branch> \
  --title "<concise, accurate title>" --body-file <description.md>
```

## 4. Make the description review-grounding

The reviewer (and `codex review` via `AGENTS.md`) needs to know what correct means. Include:

- **What this is** — one paragraph.
- **Why / evidence** — for a decision, cite sources (KVM source at the pinned tag, SDM,
  RESEARCH.md) rather than asserting; for a spike, the measured result and method.
- **What it unblocks / changes** — which tasks or contract rows this enables or refutes.
- **Cross-PR interactions** — if it conflicts with or depends on another open PR (e.g. a
  contract row that now needs redispositioning), say so explicitly. The foreman uses this
  to coordinate.
- **Determinism implications** — anything that affects bit-identical replay.

## 5. Mark ready when complete

When the work is done and any local gates pass, flip the PR to ready — **this is the signal
the foreman takes over**:

```sh
gh pr ready <pr-number>
```

From here the foreman owns it: it runs the mandatory cross-model review (`codex review`,
GPT-5.5 — the sole cross-model pass; see `.claude/skills/pr-review/SKILL.md`), iterates the
review→fix cycle until a clean cross-model pass, and **auto-merges** when
clean with green gates. Fixes are foreman-driven — directly for docs/specs, or via a spawned
fixer (`scripts/agent-takeover.sh <pr#>`) for crate code. Coordinate via PR comments; reply
to any `[question]`.
