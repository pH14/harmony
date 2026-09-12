# GitHub Actions conventions

Use `Category / Subject` display names. The allowed categories are enforced
by the `ci-workflow-prefix` lint in `scripts/custom-lints.py`.

| Category | Purpose | Trigger |
| --- | --- | --- |
| Checks | Static analysis, unit tests, API snapshots | PRs and pushes to main |
| Smoke | Short end-to-end workloads (under 15 min) | PRs and pushes to main |
| Acceptance | Long-running workload tests | Nightly schedule or manual dispatch |
| Benchmarks | Performance and search campaigns | Nightly schedule or manual dispatch |
| Nightly | Extended validation (Miri, mutation) | Nightly schedule |
| Release | Build and publish artifacts | Version tags |

Automatic per-PR jobs must finish within 15 minutes (`ci-pr-job-timeout`
lint). Short workloads that verify basic function are smoke tests. Long-running
workloads are acceptance tests that run on an off-hours schedule.

## Current workflows

| Workflow | Automatic triggers |
| --- | --- |
| Checks / Quality | PRs and pushes to main |
| Checks / Memory safety | Nightly |
| Checks / Search evaluation | Relevant PRs and changes on main |
| Acceptance / OCI | Relevant PRs and changes on main |
| Acceptance / Consonance x86 | Relevant PRs and changes on main; nightly |
| Acceptance / Workload backends | Relevant PRs and changes on main; nightly |
| Benchmarks / NES | Nightly, with parallel game/case jobs |
| Benchmarks / Historical bugs | Nightly search; bounded replay on relevant PRs |
| Release / Harmony | Version tags |

## Skill evaluation boundary

The skill evaluator is not part of this checkout. When its `benchmarks/skills/`
prerequisite lands, keep its two CI purposes separate:

| Workflow | Automatic triggers | Owns |
| --- | --- | --- |
| Checks / Skill evaluator | Relevant PRs; manual guest qualification | Sandbox, build, guest-delivery, and grading qualification without model calls. Automatic jobs stay within 15 minutes; the guest job is manual-only and may use the 45-minute ceiling when given a trusted `guest_artifact_run_id`. |
| Benchmarks / Developer skills | Nightly schedule; manual dispatch | Real-model investigation, integration, and end-to-end panels under their declared budgets. The job must fail before starting a paid attempt when provider credentials are missing. |

The no-model runner and fixture qualification belong in `Checks / Skill
evaluator`, alongside the other qualification harness checks. The benchmark
workflow should invoke the shared runner for its panels without copying those
checks or adding a real-model pull-request job. Do not add either workflow
until the evaluator sources are present on the base branch; branch-only
workflow definitions must not point at an absent `benchmarks/skills/` tree.

## Job conventions

Use `fail-fast: false` on case matrices so one failure does not cancel other
cases. Benchmark workflows end with an always-running `report` job that links
each case's status, duration, and evidence artifacts.

Hardware qualification runs separately from public self-hosted CI.
