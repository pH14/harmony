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
workloads are acceptance tests that run on an off-hours schedule. The sharded
in-diff mutation gate is the one explicit per-PR timeout exception; its named
rationale is adjacent to the 90-minute timeout in `quality.yml`.
Miri's full crate suites remain schedule/manual only (their existing 240- and
320-minute ceilings are intentionally retained). Relevant PR changes select a
per-crate Miri matrix with a 15-minute ceiling; dependency and toolchain
changes select every target. The bounded matrix reports the affected unsafe
crate directly while the scheduled suite remains the broad safety net.
Guest-backed PR smokes restore an exact cache when available and otherwise use
the most recent matching main-branch cache. Each smoke records the requested
and resolved cache keys, hit mode, fixture-source changes, and validation
scope. Prefix fallbacks verify the cached artifact manifest when one exists;
older manifestless caches require every expected artifact to be nonempty and
are reported as legacy file-presence validation. Scheduled/manual builders
publish manifest-bearing replacements. A PR that changes a guest or image
builder can therefore prove the host orchestration while the deep builder
validates the changed fixture. If no durable cache exists, the smoke preserves
a `cache-unavailable.txt` artifact and fails with the builder handoff required
to make it runnable.

Consonance's OCI platform smoke requires a verified runtime manifest. Its
scheduled/manual job builds the kernel, runtime, and tiny OCI fixture and runs
extended replay before publishing artifacts. A PR can consume that exact
artifact across branches; run the workflow manually on the proposed branch
when its source key has no qualified artifact yet. A smoke against older
verified artifacts is recorded as `host-only`. The separate guest qualification
check requires successful hardware execution with `exact-input` artifacts.
Neither missing manifests nor file presence alone qualifies this platform.
PostgreSQL uses the same runtime while retaining its application assertions.

## Current workflows

| Workflow | Automatic triggers |
| --- | --- |
| Checks / Quality | PRs and pushes to main |
| Checks / Memory safety | Relevant PRs and nightly/manual full suites |
| Checks / Search evaluation | Relevant PRs and changes on main |
| Smoke / Consonance platform | Bounded smoke on relevant PRs and main; build and extended replay nightly/manual |
| Acceptance / Consonance x86 | Bounded smoke on relevant PRs and main; deep nightly/manual |
| Acceptance / Workload backends | Bounded smoke on relevant PRs and main; deep nightly/manual |
| Benchmarks / NES | Nightly, with parallel game/case jobs |
| Smoke / PostgreSQL | Relevant PRs |
| Benchmarks / Historical bugs | Nightly/manual search and replay panel |
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
