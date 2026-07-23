# Task 139 — mutants job: survive past the ~59-minute hosted-runner preemption window

**Bead:** hm-y53x (P1, Paul directive 2026-07-21). Related: hm-4w7q (the kill evidence),
hm-jfw (the stale-install premise + re-run-across-PRs follow-up).

## Problem

Hosted `ubuntu-latest` runners running the mutants job were preempted/shut down at ~58–59
minutes **twice** on PR #134 (run `29810466534`, "runner has received a shutdown signal").
The 4-way shard fix on main sidesteps this at current diff sizes; this task addresses the
window itself so a future big-diff shard can't hit it.

## Work

1. **Diagnose with evidence, not assumption.** GitHub's documented per-job limit for hosted
   runners is far above 1 hour, so ~59 min is NOT the documented ceiling. Use `gh run list`
   / `gh run view` / `gh api` over the repo's run history (start from run `29810466534` and
   any other mutants kills you find) to discriminate between: workflow/job `timeout-minutes`
   someone already set; workflow-level `concurrency` cancel-in-progress fired by a
   subsequent push; org/repo runner policy; genuine host preemption. Write the diagnosis
   with the supporting run/log excerpts into the PR description.
2. **Implement the no-cost hardening** in the mutants job: explicit `timeout-minutes`
   comfortably above the observed window, plus a bounded retry path for *infrastructure*
   kills only — a retry must never mask a genuinely red mutants result (a shard that fails
   because mutants survived must stay red, not re-run to green).
3. **Cost-bearing options are proposals, not changes.** Larger hosted runner class or a
   self-hosted dispatchable runner for mutants: present a short costed comparison in the PR
   description for Paul to rule on. Do NOT flip the runner class yourself.

## Constraints

- **Surface list:** `.github/workflows/*` (the mutants job and only what it needs) + docs.
  No crate code.
- **Keep the diff minimal and mutants-scoped.** The `ci/gha-migration` branch is in flight
  out-of-band — do not restructure the quality workflow, rename jobs, or reformat YAML
  beyond the mutants job, or you'll manufacture conflicts.
- Validate the workflow locally (`actionlint` if installed; otherwise careful YAML review +
  `gh api` schema sanity). You cannot push, so the live GHA proof runs on the PR the foreman
  opens — say so in your IMPLEMENTATION notes and make the first-run expectations explicit.

## Acceptance

- Diagnosis written up with run-log evidence (which hypothesis survived, which were ruled
  out and by what).
- Mutants job carries explicit `timeout-minutes` + infrastructure-only retry that cannot
  convert a real red into a green.
- Costed runner-class/self-hosted options table for Paul.
- Portable gates green for anything touched (for a CI-only diff that is fmt/deny no-ops —
  run them anyway; they're cheap).

## Environment

Mac-local only (gh CLI + workflow files). No box, no Nimbus.
