# GitHub Actions conventions

Every workflow uses a unique `Category / Subject` name, including PR workflows:
`Checks / Repository`, `Checks / Memory safety`, and `Checks / Product smokes`.
Every job uses `Role / Subject`: `Test / Native NES search`,
`Build / Linux guest image`, or `Report / NES campaign report`.
Allowed job roles are `Check`, `Test`, `Build`, `Search`, `Replay`, `Report`, and
`Publish`. Roles describe the job's primary purpose; setup and artifact steps
stay in their owning job. Job names must be unique within a workflow.

Subjects use sentence case: capitalize the first word, proper names, and
acronyms only. Generic subjects such as `Products`, `Quality`, and `Report` are
rejected. Matrix expressions are allowed only in job subjects, alongside static
descriptive text. `ci-workflow-prefix` and `ci-display-name` enforce the naming
contract; register proper names/acronyms in `CI_NAME_TERMS`.

| Category | Purpose | Trigger |
| --- | --- | --- |
| Checks | Bounded static checks, unit tests, proofs, Miri, and product smokes | PRs and pushes to main, according to each workflow's routing |
| Validation | Deeper correctness and replay qualification, full Miri, coverage, mutation | Schedule or manual dispatch |
| Benchmarks | Performance and search capability measurements | Schedule or manual dispatch |
| Release | Build and publish artifacts | Version tags |

Nightly describes a schedule, not a category. Smoke describes a bounded test,
not a separate workflow category. Validation asks whether execution and replay
are correct; Benchmarks measures how much search or performance a budget buys.
An unsolved search case can be valid benchmark evidence; missing or incorrect
replay evidence cannot qualify a runtime.

Automatic per-PR jobs must finish within 15 minutes (`ci-pr-job-timeout`
lint). Short workloads that verify basic function are smoke tests. Long-running
correctness tests run in Validation on a schedule or manual dispatch. Timeout
exceptions in comments are not accepted. Coverage and mutation belong in Validation.
Miri's full crate suites remain schedule/manual only (their existing 240- and
320-minute ceilings are intentionally retained). Relevant PR changes select a
per-crate Miri matrix with a 15-minute ceiling; dependency and toolchain
changes select every target. The static bounded matrix lists every registered
crate, but only affected crates install or run Miri. Unselected entries finish
as explicitly reported not-applicable jobs. The scheduled suite remains the
broad safety net.
Guest-backed PR smokes consume verified cached artifacts or an exact artifact
handoff from the scheduled/manual builder. Changed guest inputs require an
exact build and qualification before the platform smoke can qualify the PR.
The expensive builder runs through manual dispatch on the proposed branch.
Missing artifacts fail the smoke; they are never treated as passing evidence.

Consonance's OCI platform smoke requires a verified runtime manifest. Its
scheduled/manual job builds the kernel, runtime, and tiny OCI fixture and runs
extended replay before publishing artifacts. A PR can consume that exact
artifact across branches; run the workflow manually on the proposed branch
when its source key has no qualified artifact yet. A smoke against older
verified artifacts is recorded as `host-only`. The separate guest qualification
check requires successful hardware execution with `exact-input` artifacts.
Neither missing manifests nor file presence alone qualifies this platform.
PostgreSQL uses the same runtime while retaining its application assertions.
The platform smoke also verifies the controlled minimal fixture and requires two
clean same-seed Linux boots with identical execution logs. The KVM smoke runs
the fixed-core published snapshot/replay identity matrix, including fresh and
reused vCPUs, extra host entries and floating-point/vector negative controls.
Broader snapshot hardware cohorts remain in Validation / Consonance x86.

## Current workflows

| Workflow | Automatic triggers |
| --- | --- |
| Checks / Repository (`quality.yml`) | PRs and main; lint/build/unit tests, Kani proofs, and public API compatibility |
| Checks / Memory safety (`nightly.yml`) | Relevant PRs; bounded memory-safety checks |
| Checks / Product smokes (`product-smoke.yml`) | PRs and main; selected native NES, STB, PostgreSQL, VM, and KVM checks |
| Validation / Memory safety | Nightly/manual full Miri suites |
| Validation / Extended quality | Nightly/manual coverage and full-tree mutation |
| Validation / Search evaluation | Manual common-runner and STB qualification |
| Validation / Consonance platform | Nightly/manual exact build and extended replay |
| Validation / Consonance x86 | Nightly/manual hardware and determinism suites |
| Validation / Workload backends | Nightly/manual backend and nested-runtime suites |
| Validation / Kernel XSAVE qualification | Manual kernel XSAVE fixture and replica checks |
| Benchmarks / NES | Nightly/manual independent public case jobs and aggregate roster |
| Benchmarks / Historical bugs | Nightly/manual search and replay panel |
| Release / Harmony | Version tags |

`scripts/ci_scope.py` is the single selector for product smokes. Search-core and
ordinary NES changes select the native NES smoke; STB-specific changes select
STB. Fault changes select PostgreSQL. Platform changes select the OCI smoke;
VMM/backend changes also select the hardware execution/restore/replay smoke.
Shared process interfaces and CLI changes select the relevant consumers.
Dependency, toolchain, and selector changes conservatively select all consumers.
Documentation changes do not select product smokes. Portable quality checks run
in six independent jobs: repository scripts and custom lints, workspace lint
and unit tests, guest crates, Dissonance search, workload support, and NES
packages. A seventh independent job judges changed file content with the
Jev-backed semantic lint. It uses the `Checks` environment, limits secret access
to the judge step, and checks out two commits for the first-parent diff.
Semantic lint unit tests remain in the portable repository checks. Each
quality job retains the 15-minute bound and runs on every PR and main push. Rust jobs cache their own manifest directories under separate keys;
there is no dependency chain or shared build-artifact handoff. Keeping lint,
build, and tests for each manifest together reuses compilation within a job.
All six jobs are required quality evidence alongside applicable proofs and API
checks. Public-API checks run for
platform/dependency changes; proof and Miri selection retain their own narrow
rules and tests.

There are no standalone selection checks. Each smoke, Kani, public API, and
Miri job checks out full commit history with `filter: blob:none` and invokes `.github/actions/ci-scope` as its
first local step. `scripts/ci-job-scope.py` computes the same complete Git diff
as the previous routing jobs and delegates to the existing selectors. It does
not use GitHub's changed-file API or introduce new native path-filter limits.
Rename detection is disabled so moves select checks for both source and
destination paths without fetching historical blobs for rename scoring.
The blob filter avoids downloading every historical file revision into every
runner; the current working tree is materialized, and Git can fetch old blobs
on demand if a diff needs them. Changes to the shared routing implementation
conservatively select all tests. Miri and Kani selector changes also select
their own checks; the Miri workflow trigger includes its selector and tests.

Every subsequent setup, test, cache, and artifact step is guarded by that job's
selection output. A selected test failure still fails its job and still uploads
available failure evidence. A diff/selection error fails closed. Unselected
jobs briefly allocate a runner and finish successfully with a summary saying
`Not applicable` and `Test steps were not run`; they do not install tools,
restore caches, run tests, or upload empty artifacts. This preserves test
selection, not the old skipped-job status or runner allocation behavior.
Exact guest qualification remains a real evidence check after selected
platform execution; a successful host-only smoke cannot qualify guest inputs.

Nova-through-Consonance search is a scheduled/manual validation
campaign, not a separate PR smoke. Native NES exercises the shared search loop;
the faults and platform smokes cover the Consonance execution path on PRs.

Full-tree mutation runs in sixteen nightly shards with a 320-minute ceiling;
the coverage floor remains 90%. It no longer depends on a PR diff. These jobs
can be dispatched before merge when deeper evidence is needed. They are not
automatic PR requirements.

NES uses one independent job per registered case, retaining all three seeds in
each job. Nova whole-game work cannot delay or fail the STB jobs. Each case
uploads its own compact export; `scripts/nes-nightly-report.py` combines the
rosters and links the complete case reports. Missing, duplicate, or mismatched
evidence fails the report while retaining a visible row for every expected
cell. Search errors retain their original status and fail the owning job.
The case-job ceiling is 210 minutes: the three whole-game seeds need two
CPU-admission waves (up to 114 minutes including finish budgets), plus cold
builds and evidence export. The ceiling does not increase any search budget.

The historical panel remains the single owner of PostgreSQL and etcd searches.
Do not introduce a second case-specific workflow. GitHub retains workflow
registry entries from branch-only runs even when their file is absent on main;
inspect `gh workflow list --all` and run history before disabling an obsolete
entry. Disabling preserves its old runs and is separate from repository lint.

## Coverage ownership

| Repeated component | PR evidence | Deeper evidence | Measurement |
| --- | --- | --- | --- |
| Memory safety | Selected bounded per-crate Miri | Full crate suites in Validation / Memory safety | None |
| NES search | Bounded native and STB smoke | Validation / Search evaluation verifies full small-campaign replay, cartridge RAM snapshots, and the STB contract; Validation / Workload backends verifies guest/native equivalence and replicated VM search | Benchmarks / NES runs the registered multi-seed capability panel |
| Consonance platform | Execution/restore and exact guest artifact consumption | Validation / Consonance platform owns exact builds and extended replay; Validation / Consonance x86 owns broad hardware cohorts | Historical bug searches consume the qualified runtime |
| PostgreSQL faults | Bounded single-case search and replay | Runtime qualification belongs to the platform suites | Benchmarks / Historical bugs owns the multi-case search panel |

These layers share components but have different assertions, origins, and
budgets. The manual search qualification is not another scheduled capability
panel. Full Miri and broad hardware suites extend selected PR coverage. Keep
runtime qualification in Validation and capability measurements in Benchmarks;
reuse shared runners rather than copying their implementations.

## Skill evaluation boundary

The skill evaluator is not part of this checkout. When its `benchmarks/skills/`
prerequisite lands, keep its two CI purposes separate:

| Workflow | Automatic triggers | Owns |
| --- | --- | --- |
| Checks / Repository (Skill evaluator job) | Relevant PRs | Bounded sandbox, build, guest-delivery, and grading checks without model calls; each job stays within 15 minutes. |
| Validation / Skill guest qualification | Manual dispatch | Guest qualification with a trusted `guest_artifact_run_id`, in a separate workflow with a 45-minute ceiling. |
| Benchmarks / Developer skills | Nightly schedule; manual dispatch | Real-model investigation, integration, and end-to-end panels under their declared budgets. The job must fail before starting a paid attempt when provider credentials are missing. |

The no-model runner and fixture qualification belong in a `Test / Skill evaluator`
job under `Checks / Repository`, alongside the other qualification harness checks. The benchmark
workflow should invoke the shared runner for its panels without copying those
checks or adding a real-model pull-request job. Do not add these workflows
until the evaluator sources are present on the base branch; branch-only
workflow definitions must not point at an absent `benchmarks/skills/` tree.

## Job conventions

`scripts/custom-lints.py` parses every workflow using PyYAML 6.0.3. Missing parser
dependencies, invalid YAML, duplicate mapping keys, and invalid trigger shapes
fail validation. CI rules cannot be waived through the lint baseline.

`ci-workflow-triggers` confines Validation and Benchmarks to schedule,
manual, or reusable invocation. Checks cannot use schedules.
`ci-pr-only-jobs` rejects nightly/manual jobs embedded in PR workflows even when
an event guard skips them. The timeout rule includes pull_request_target and
merge_group; bounds must be positive and at most 15 minutes.
`ci-pr-extended-validation` rejects direct cargo-mutants and cargo-llvm-cov
commands and the known `scripts/coverage.sh` wrapper in PR jobs, including jobs
with short timeouts. `ci-pr-workflow-registration` requires PR workflows to use
registered file paths and categories; adding a new PR workflow is an explicit
contract change, not a way to bypass smoke routing under a Checks name.
`ci-pr-smoke-routing` requires every smoke to use the shared inline selector
with its registered consumer identity. `ci-pr-check-routing` does the same for
Kani, public API, and Miri checks, and compares the static Miri matrix against
the registered targets. Both enforce blob-filtered full-history checkout and selection
guards on every setup/test/artifact step. Separate routing jobs, unguarded
steps (including `always()` uploads), and missing Miri targets fail validation.
Each smoke consumer is one bounded job; added smoke matrices fail validation.

`ci-nes-case-jobs` compares the Benchmarks / NES case matrices to
`benchmarks/search/nightly.json` and requires its owning `nova-nightly.yml`
workflow to remain tracked while the manifest exists. Each case must occur exactly once in a static
`matrix.case` list, run through `eval.py run` with `--case ${{ matrix.case }}`,
and use `fail-fast: false`. Adding a game to the public roster therefore requires
adding its cases to the workflow. The report must run with `always()` and depend
on every campaign matrix.

Run the regression tests with `python3 -m unittest discover -s scripts -p 'test_custom_lints.py'`
and `python3 -m unittest discover -s scripts -p 'test_ci_*.py'`.
They include the old monolithic panel, omitted and
duplicated cases, mixed triggers, short-timeout expensive commands, YAML parse
failures, the removed timeout exemption, sentence-case display names, and
inline-selection bypasses. Selector parity tests cover docs-only and mixed
changes, large diffs, Miri arguments/flags, and diff failures.
The lint/build/unit-test job runs them
before invoking the linter.

These checks validate declared workflow structure. They do not determine the
cost of arbitrary scripts, prove the correctness of change selectors, or inspect
GitHub's retained registry of branch-only workflows. Selector tests and a
separate registry audit remain necessary for those boundaries.

Use `fail-fast: false` on case matrices so one failure does not cancel other
cases. Benchmark workflows end with an always-running `report` job that links
each case's status, duration, and evidence artifacts.

Hardware qualification runs separately from public self-hosted CI.
