# GitHub Actions conventions

Workflow display names use `Category / Subject`:

- Checks: code correctness, static analysis, and memory safety.
- Acceptance: product behavior, backend compatibility, and determinism.
- Benchmarks: reproducible measurements and comparisons over time.
- Showcase: demonstrations, films, and published examples.
- Release: packaging and distribution.

Use subjects rather than schedules or machine names. Keep existing workflow
filenames and job identifiers stable during display-name cleanup: callers,
badges, and required checks may depend on them. New filenames use lowercase
category-subject.yml. Games, seeds, budgets, and backends usually belong in
job matrices or manual inputs. Separate workflows when execution policy or
purpose differs. Keep hardware qualification out of public self-hosted CI.

The NES showcase currently combines search with rendering. Separate benchmark
measurement from presentation when the common evaluation suite lands. Incoming
evaluation contract checks belong under Checks; repeatable search measurements
under Benchmarks / Dissonance. Consonance acceptance and performance measurements
belong in Acceptance and Benchmarks respectively.

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
