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

## Job conventions

Use `fail-fast: false` on case matrices so one failure does not cancel other
cases. Benchmark workflows end with an always-running `report` job that links
each case's status, duration, and evidence artifacts.

Hardware qualification runs separately from public self-hosted CI.
