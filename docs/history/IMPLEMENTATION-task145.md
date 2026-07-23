# tasks/145 — landing the GHA-migration + skills/hygiene residue (hm-nsfl)

**Branch:** `task/gha-residue-landing` (cut from `origin/main`). **Residue source:**
`origin/oob/gha-residue` (left untouched; `origin/ci/gha-migration` also untouched).

This doubles as the PR-description write-up (conventions: multi-directory task → write-up
lives in the review record). It covers the four items the spec asks for: the sharding
reconciliation decision, the rename-discrepancy note, the post-merge-only validation list,
and a file-by-file map of the evacuation snapshot.

## TL;DR — the task premise changed under us

The spec was written assuming the residue's **CI migration was new work to land** (with a
sharding conflict to resolve). It is not. Between the residue branch going stale (last
main-merge `73abc909`, 2026-07-17) and today, **main independently landed the same
migration and more**:

- **PR #118** (`64c884d5`, *"ci: migrate content-check gates from the self-hosted box to
  GitHub-hosted runners"* — byte-for-byte the same commit subject as the residue's
  `cdb4fb9e`): `quality.yml` **and** `nightly.yml` already run on `ubuntu-latest`.
- **PR #141 / tasks/139** (`bc632bed`): mutants job has `timeout-minutes: 90` + infra-only
  retry (never retries a real red).
- mutants 4-way sharding (`a28da42a`, tasks/132) is already on main.
- **PR #133** (`80332d14`): the `guest/` → `harmony-linux/` crate rename.
- `cargo-deny@0.19.9` is already pinned in `quality.yml` (three sites) — i.e. the residue's
  `858a40e3` is already on main too.

So **both CI commits in the residue are fully superseded by main**, and cherry-picking them
would be a pure regression (see Constraint 1). The genuinely-new residue content is the
**skills / Nimbus / docs** subset of the evacuation snapshot `d818c5e7`. That is what this
branch lands.

## Disposition of the three "real" commits

| Commit | What it is | Disposition |
|---|---|---|
| `cdb4fb9e` | ci: migrate content-check gates → hosted runners | **Superseded by main PR #118 — not landed.** Its `quality.yml`/`nightly.yml` are a pre-#133/#141 snapshot; landing them would revert the rename, un-shard mutants, drop `timeout`/retry, and delete the `differential-lineage`, `AA-1(a) EL0 evidence`, and `determinism comparator` gates main has since added. |
| `858a40e3` | ci: pin `cargo-deny@0.19.9` | **Superseded by main — not landed.** Main already pins `cargo-deny@0.19.9`. |
| `d818c5e7` | evacuate uncommitted working tree | **Partially landed.** Only the genuinely-new/additive subset (below). Much of it is already on main via other landings. |

Authorship of all three is Paul (`785446+pH14@users.noreply.github.com`); the landing commit
preserves it via `--author`.

## Constraint 1 — mutants sharding reconciliation (the decision)

The spec requires the landed `quality.yml` to keep **both** the 4-way sharding **and** the
PR #141 `timeout` + infra-only-retry structure, with any deviation argued as a costed option.

**Decision: keep main's `quality.yml` and `nightly.yml` verbatim — no change.** Main's
current `quality.yml` *is already* the exact required end-state (hosted runners + 4-way
shard + `timeout-minutes: 90` + infra-only retry + `cargo-deny@0.19.9`). The residue's
version is strictly older and would regress all of it. This is the spec's stated **default**
("keep both"), reached by making zero edits to the workflow rather than by re-deriving it —
so there is no costed deviation to argue. No CI file is touched by this PR.

## Constraint 2 — crate-tree guard + the rename discrepancy

- **No crate rename performed, either direction.** `git diff origin/main -- harmony-linux
  guest` over the landed tree is empty; the `harmony-linux/` tree matches main exactly.
- **No `guest/` path references landed.** The only `guest/` references on the residue branch
  live in `cdb4fb9e`'s stale workflow YAML (authored before PR #133's rename) — and those
  commits are not landed. Every file this PR does land was grepped: no added line references
  a `guest/` path.
- **Bead `hm-nsfl` item (b) discrepancy (recorded per spec, for Paul):** the bead describes a
  *"harmony-linux→guest crate rename (committed on the branch)."* **Git shows no such
  commit.** `git log main..origin/oob/gha-residue` is: `cdb4fb9e`, `858a40e3`, three
  `origin/main` merge commits, and `d818c5e7` — none renames the crate tree. The only
  `guest/`-vs-`harmony-linux/` divergence is that the residue's **workflow YAML predates**
  PR #133 and still *names* `guest/` paths; that is staleness, not an intended rename-back.
  Per Constraint 2 this is a discrepancy to record, and no "actual evidence of an intended
  rename-back" (the STOP/escalate trigger) was found — so I proceeded without escalating.

## Constraint 3 — CI is validated by CI + post-merge-only list

This PR makes **no workflow changes** (the migration is already live on main), so the
"CI edits are validated by the PR's own run" clause has nothing to validate — there is no
migrated job to green-check that main isn't already running. Consequently:

- `actionlint` / YAML sanity on workflows: **N/A** — no workflow file is modified.
- **Post-merge-only validation:** none. Nothing in this PR can only be validated post-merge
  (no `nightly.yml` schedule paths, no push-to-main-only jobs are touched).
- What *was* validated locally (below): the secret-hygiene scanner on the landed files, skill
  symlink resolution, skill/`openai.yaml` YAML well-formedness, and harness skill discovery.

## File-by-file map of the evacuation snapshot (`d818c5e7`, 33 paths)

**Landed verbatim (genuinely new — absent from main):**
- `.agents/skills/harmony-coordinator/{SKILL.md,agents/openai.yaml}`
- `.agents/skills/harmony-handoff/{SKILL.md,agents/openai.yaml}`
- `.agents/skills/harmony-pr-review/{SKILL.md,agents/openai.yaml,references/seats/*.md}` (10 seats)
- `.agents/skills/nimbus/SKILL.md`
- `.claude/skills/harmony-coordinator` → `../../.agents/skills/harmony-coordinator` (symlink)
- `.claude/skills/harmony-handoff` → `../../.agents/skills/harmony-handoff` (symlink)
- `.claude/skills/harmony-pr-review` → `../../.agents/skills/harmony-pr-review` (symlink)
- `.claude/skills/nimbus/SKILL.md` (discovery shim → the `.agents` authority)
- `docs/CLI.md` (the parked DRAFT CLI/plugin grammar)
- `docs/NIMBUS.md` (the Nimbus execution-boundary doc)

**Landed as additive hunks only (kept main's content, appended the Nimbus paragraphs):**
- `.claude/skills/foreman/SKILL.md` — added 3 Nimbus paragraphs; **kept main's post-07-17
  arrivals-surfacing block** (the residue would have deleted it — a stale revert of the
  2026-07-17 secret-guardrails lesson).
- `scripts/agent-spawn.sh` — added the Nimbus worker-prompt paragraph.
- `tasks/00-CONVENTIONS.md` — added the Nimbus authorization paragraph to the Environment
  section.

**Not landed — already on main (identical, or landed via another path):**
- `.pre-commit-config.yaml`, `README.md`, `scripts/install-quality-tools.sh` — identical to main.
- `tasks/106-cloud-vendor-cli.md` removal — **already removed on main** (the removal is a
  no-op here; not re-added, per Non-goals).

**Not landed — main's version is newer/superior (residue hunk is a stale revert):**
- `.github/workflows/quality.yml`, `.github/workflows/nightly.yml` — see Constraint 1.
- `.github/workflows/secret-scan.yml` — main has the F1/F2/F4-hardened version
  (range-safe concurrency groups, empty-range full-history scan, fail-closed on unfetchable
  rewind tips); the residue has the older simple version.
- `.githooks/pre-commit` — main has the beads-hook delegation (compose-don't-clobber);
  the residue would remove it.
- `docs/SECRET-HYGIENE.md` — main documents the beads-interaction section matching the hook
  above; the residue would remove it.

## Skills-duplication decision

The three `harmony-*` skills are **draft provider-neutral twins** that deliberately coexist
with main's ruled Claude-native `foreman` / `handoff` / `pr-review`. Each carries an explicit
`**Status: draft.**` header ("Do not replace the currently ruled foreman process… unless the
user explicitly invokes or adopts it") and each `openai.yaml` sets
`allow_implicit_invocation: false`, so they are inert until explicitly invoked. Landing them
alongside — **not** in place of — the originals is therefore the intended state; no existing
skill is deleted or renamed. This extends the `.agents/skills/` provider-neutral convention
that main already established for `beads`. `harmony-pr-review` bundles its own copy of the
review seats under `references/seats/` (9/10 identical to `.claude/skills/pr-review/seats/`;
only `judge.md` is adapted) so codex/OpenAI agents can use it without reaching into
`.claude/`.

## Validation performed

- `detect-secrets` (the tracked pre-commit hook — the secret-hygiene stack's own scanner):
  **Passed** on all 26 landed files. No secret material in the residue.
- Skill symlinks resolve (`.claude/skills/harmony-* → .agents/skills/harmony-*/SKILL.md`),
  and git stores them as symlinks (mode `120000`); the nimbus discovery-shim relative path
  resolves.
- `SKILL.md` frontmatter + `openai.yaml` are well-formed (2-space YAML, no tabs); the Claude
  Code harness discovered and loaded all four new skills.
- No Rust touched → no cargo gates apply.

## Deviations considered

- **Cherry-pick → selective land.** The spec recommends cherry-picking the three commits.
  Because main superseded two of them and part of the third, a verbatim cherry-pick would
  regress main; I instead landed only the genuinely-new/additive subset and hand-merged the
  three shared files. Authorship preserved via `--author`.
- **No docs/history file vs. this file.** Given how far the outcome diverges from the spec's
  premise, a durable in-repo reconciliation record is genuinely warranted (conventions allow
  `docs/history/IMPLEMENTATION-task<NN>.md` for exactly this).

## Handoff notes for the foreman

- Branch is committed, **not pushed** (per worker prompt). Open the PR as
  *"tasks/145: land GHA-migration + skills/hygiene residue (hm-nsfl)"*; this file is the
  ready-to-paste description.
- Main advanced by one unrelated commit during this session (`d0a25af4`, a foreman QUEUE
  update recording this spawn). A rebase onto current `origin/main` is clean (no overlapping
  paths).
- Tribunal tier is the foreman's call; there is no Rust and no CI change, so this is a
  docs/skills-only PR.
