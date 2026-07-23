# Task 145 — Land the GHA-migration + skills/hygiene residue (hm-nsfl)

**Bead:** `hm-nsfl` (P1, Paul-directed 2026-07-22). **Branch to land:** `origin/oob/gha-residue`
(leave `origin/ci/gha-migration` untouched — it stays as history). **Work branch:**
`task/gha-residue-landing`, cut from current `origin/main`.

## What the residue branch contains

Six commits unique to the branch (`git log main..origin/oob/gha-residue`):

- `cdb4fb9e` — ci: migrate content-check gates from the self-hosted box to GitHub-hosted
  runners (`.github/workflows/quality.yml`, `nightly.yml`).
- `858a40e3` — ci: pin cargo-deny@0.19.9 (latest release broke `check --config` CLI).
- three `origin/main` merge commits (last main-merge = `73abc909`, 2026-07-17 — the branch
  is ~a week stale).
- `d818c5e7` — evacuation snapshot (previously-uncommitted work): provider-neutral skills
  (`.agents/skills/harmony-coordinator|harmony-handoff|harmony-pr-review|nimbus` +
  `.claude/skills/*` symlinks), secret-hygiene stack (`.githooks/pre-commit`,
  `.pre-commit-config.yaml`, `.github/workflows/secret-scan.yml`, `docs/SECRET-HYGIENE.md`),
  `docs/CLI.md`, `docs/NIMBUS.md`, foreman `SKILL.md` / `agent-spawn.sh` /
  `install-quality-tools.sh` / `tasks/00-CONVENTIONS.md` edits, `tasks/106-cloud-vendor-cli.md`
  removal (Paul-directed; do not re-litigate).

**Recommended mechanics:** cherry-pick the three real commits (`cdb4fb9e`, `858a40e3`,
`d818c5e7`) onto the fresh work branch rather than rebasing through the merge commits.
Preserve authorship; resolve conflicts per the constraints below.

## Constraint 1 — the mutants-sharding regression (PR #141 §3, merge-blocking)

As written, `cdb4fb9e` UN-SHARDS the mutants job — a preemption-survivability regression.
Main has since merged (PR #141, tasks/139): `timeout-minutes: 90` + infra-only retry
(never retries a real red), and main's mutants job is 4-way sharded (tasks/132).
**Resolution required:** the landed `quality.yml` keeps BOTH the 4-way sharding AND the
PR #141 timeout+infra-retry structure inside the hosted-runner migration. Any deviation
(e.g. deliberately re-tuned timeout instead of sharding) must be argued explicitly in the
PR description as a costed option — default is keep both.

## Constraint 2 — crate-tree guard (no renames)

The branch predates main's `guest/` → `harmony-linux/` rename (PR #133, 2026-07-21); none
of its unique commits touch either path. The landed result must match main's
`harmony-linux/` tree EXACTLY — perform **no** crate rename in either direction. If any
residue file (workflow, script, skill, doc) references `guest/` paths, update those
references to `harmony-linux/`. Note: bead `hm-nsfl` item (b) describes a
"harmony-linux→guest crate rename (committed on the branch)" — git shows no such commit;
record this discrepancy in the PR description for Paul rather than acting on it. If while
landing you find actual evidence of an intended rename-back, STOP and escalate instead of
guessing.

## Constraint 3 — CI changes are validated by CI

This PR edits the quality gates themselves, so the PR's own GitHub run is the decisive
gate: push and confirm every migrated job goes green on hosted runners (content-check
gates, cargo-deny with the 0.19.9 pin, secret-scan, mutants shards within timeout). Run
`actionlint` locally if available (`brew install actionlint` is fine) plus YAML sanity
before pushing. Anything that can only validate post-merge (e.g. nightly.yml schedule
paths) gets named explicitly in the PR description with the reason.

## Also verify

- `.claude/skills/*` symlinks into `.agents/skills/` survive the cherry-pick and resolve.
- No secret material anywhere in the residue (the secret-hygiene stack itself must pass
  its own scan); `.githooks/pre-commit` documented activation (`core.hooksPath`) matches
  what `docs/SECRET-HYGIENE.md` says.
- `scripts/agent-spawn.sh` / `install-quality-tools.sh` / foreman-skill edits do not
  conflict with the versions merged to main since 07-17 — reconcile by keeping main's
  behavior unless the residue's edit is additive.
- Standard gates for anything Rust-touching (there should be none): fmt, clippy
  `-D warnings`, nextest.

## Deliverable

A **ready** (non-draft) PR from `task/gha-residue-landing`:
"tasks/145: land GHA-migration + skills/hygiene residue (hm-nsfl)". Description covers:
the sharding reconciliation decision, the rename-discrepancy note, the post-merge-only
validation list, and a file-by-file map of the evacuation snapshot. Normal review
machinery from there (tribunal tier is the foreman's call at review time).

## Non-goals

- No crate code changes beyond mechanical path-reference fixes.
- No changes to `ci/gha-migration` (history) and no deletion of any origin branch.
- No re-adding `tasks/106-cloud-vendor-cli.md` (its removal is Paul-directed; the CLI
  program is out-of-band — see bead `hm-6ge` history).
