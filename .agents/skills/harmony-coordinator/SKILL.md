---
name: harmony-coordinator
description: Draft the next provider-neutral Harmony coordinator workflow. Use when explicitly evaluating or exercising the proposed cross-vendor orchestration that reconciles Beads, GitHub PRs, branches, workers, opposing-vendor review, and bounded project advancement.
---

# Harmony coordinator

**Status: draft.** Do not replace the currently ruled foreman process or perform a live merge
under this workflow unless the user explicitly invokes or adopts it.

Advance the project by one bounded iteration. Do not assume the coordinator, worker, or
reviewer runs on a particular model or agent harness.

## Ownership of state

- Beads owns work items, priority, dependencies, blockers, and durable project memory.
- GitHub owns PR, review, commit, draft, and merge state.
- Git and the active agent harness own branch, worktree, and live-session observations.
- Derive the current pipeline stage from those sources. Never create a parallel task store.

Rebuild the picture every iteration. Treat any status command or future snapshot helper as
a read-only join, not an authority.

## Provider rule

Use the agent mechanism available in the current environment. A provider adapter may be an
interactive subscription session in tmux, a native subagent, or a one-shot CLI. Do not
require headless API execution merely to normalize providers.

Whenever an agent materially changes implementation code, preserve its vendor and model in
the PR provenance marker defined by `$harmony-handoff`. Before substantive review, route the
discovery panel to a vendor that did not implement the change:

- Anthropic-only implementation -> OpenAI review.
- OpenAI-only implementation -> Anthropic review.
- Human-only implementation -> use the configured default panel; prefer both vendors for a
  high-risk PR.
- Mixed Anthropic/OpenAI implementation -> disclose that no two-vendor opponent remains and
  use fresh independent contexts from both vendors. Never claim this is opposing-vendor review.

Model names are defaults, not process invariants. Vendor opposition, fresh context,
blindness, evidence, and bounded review are the invariants.

## One iteration

1. Read `AGENTS.md`, then run `bd prime` if Beads context is absent or stale.
2. Observe `bd ready --json`, open GitHub PRs, relevant PR reviews and head commits, local
   and remote task branches, live worker sessions, and lifecycle markers.
3. Skip draft PRs. Explicitly include ready non-task handoff PRs.
4. Derive each item stage:
   - `unstarted`: ready Bead, no task branch, no worker.
   - `in-progress`: live worker, no ready PR.
   - `needs-pr`: pushed branch, no worker, no PR.
   - `needs-review`: ready PR without a current-head coordinator verdict.
   - `needs-fix`: blocking verdict on current head, no fixer.
   - `fixing`: live fixer.
   - `needs-verification`: fixed head newer than the blocking verdict.
   - `mergeable`: green required gates and zero open P1 findings on current head.
   - `done`: merged.
5. Perform cheap reconciliations, then at most one heavy review or verification event.
6. Act in this order: merge; verify; review; dispatch fixes; open missing PRs; nudge one
   stalled worker; dispatch ready work up to the authorized concurrency limit.
7. Update Beads only for durable truth: close merged work, create discovered follow-ups,
   and repair real dependency edges. Do not mirror transient PR/session stages into Beads.
8. Report stages, actions, provenance/review routing, escalations, and the next wake time.

## Dispatch

Choose workers by task risk and capability, not brand loyalty. Give every worker the task
spec, `tasks/00-CONVENTIONS.md`, its existing worktree/branch, required gates, and explicit
authority boundaries. For interactive Claude Code subscription workers, the existing tmux,
send, and Stop-hook machinery is valid. Other providers may use their native lifecycle.

Record the actual provider after dispatch; never infer it later from branch naming.

## Review and merge

Use `$harmony-pr-review` for substantive PRs. Light docs/spec/handoff PRs receive a coordinator
sanity read and applicable gates without a tribunal. Merge only when portable gates, any
required box gate, and the review disposition are green. Keep the review event cap from the
binding tribunal ruling unless the user explicitly changes it.

Escalate contradictions requiring an integrator ruling, repeated failures on the same root
cause, broken infrastructure, unavailable required opposing-vendor review, or an empty queue.
