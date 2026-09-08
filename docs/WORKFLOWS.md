# GitHub Actions conventions

Use `Category / Subject` display names. Every entry-point workflow has an
automatic trigger; manual dispatch is optional. Keep existing filenames and
required-check job identifiers stable where possible.

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

Benchmark workflows end with an always-running `report` job. It links each
case's status, duration, and evidence artifacts, including failed cases. Existing
case artifacts carry detailed outcomes, resource measurements, replay evidence,
and videos where supported. Reports never turn a failed case into a success.
There is no separate showcase workflow and reporting does not rerun search.
The current game executables produce videos alongside their evaluation artifacts;
the downstream report collects links to those outputs.

Use fail-fast: false on case matrices so one failure does not cancel other
cases. Matrices share the owner's hosted-runner concurrency allowance with other
workflows. Optional manual selectors narrow a multi-game suite; a Nova-only checkout
runs Nova for both all and nova.
ROM-dependent games stay in the local evaluation harness. New historical cases
join the existing suite once implemented; documentation alone is not a runnable
case. Known-bug replay and fixed-version checks stay inside that suite.

Hardware qualification runs separately from public self-hosted CI.

## Credentials

The repository-managed pre-commit hook runs pinned detect-secrets via
.pre-commit-config.yaml. It scans staged files offline, with credential
verification disabled. Hex entropy detection is disabled because the repository
contains many legitimate content hashes; other configured detectors remain active.
Run scripts/install-quality-tools.sh to configure hooks, then install pre-commit
and run pre-commit install-hooks as instructed. Do not replace core.hooksPath
with a separate hook installation.

GitHub native secret scanning and repository push protection provide centralized
detection and blocking for supported token types. Neither covers every secret;
local hooks may also be bypassed. A published credential must be revoked.

TruffleHog is not required for normal commits. Its recommended pre-commit mode
uses credential verification, which does not fit this repository's offline hook
policy. A separate scanner should be added only for a demonstrated detection gap,
with staged-file coverage, worktree behavior, latency, and network policy tested.
