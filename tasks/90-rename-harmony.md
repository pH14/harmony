# Task 90 — project rename & regrouping: *harmony* / *consonance* / *dissonance* / *unison*

> **Close-out note (task 62, 2026-07-01).** This rename is **~95% executed**, not fully pending as
> the header below still implies: the source layout, crate names, docs, and skills already use
> `harmony`/`consonance`/`dissonance`/`unison` throughout. The real straggler is narrow and
> **deliberate**: the `hypervizor` strings at `guest/linux/lib-build.sh:48,59,60` are **hashed
> `MANIFEST.sha256` build inputs** (the task-43 landmine — changing them invalidates the manifest
> and forces a rebaseline). **Integrator ruling: document-as-deliberately-stale**, not
> rebaseline-now. A comment sits at each site pointing back here; the strings themselves are
> untouched. If a rebaseline is ever scheduled for other reasons (e.g. task 43's `harmony-linux`
> tier landing), fold this rename in as part of it rather than doing it standalone.

> **DEFERRED CHORE · PLANNED FREEZE-WINDOW · DO NOT AUTO-SPAWN.** This is a tree-wide mechanical
> rename that conflicts with **every** in-flight branch. The foreman must **not** pick it up in the
> normal numeric auto-spawn flow. Numbered 90 to keep it out of the day-to-day queue.
>
> **Sequencing (integrator directive, 2026-06-23):** getting things **running on the box (M1)** is
> the priority and comes first. Once M1 / the first real box runs land *and the queue is otherwise
> clear*, this is worth a deliberate **blocking step (ideally overnight)**: pause new work, drain
> open PRs, push the rename + code movement through as one atomic change, **before** starting the
> next coding wave (which will be written against the new names/structure). So the foreman should
> **proactively propose this freeze window when M1 is achieved and no task PRs are open** — not
> leave it indefinitely, and not run it mid-bring-up. Aesthetic/organizational only — zero
> runtime-behavior change.

Read `tasks/00-CONVENTIONS.md` first.

## The naming scheme (from the integrator)

A musical-harmony theme for the **top-level projects**, while the **subsystems keep their
technical names**:

| Old | New | What it is |
|---|---|---|
| `hypervizor` | **harmony** | the overall project / repo / workspace |
| the deterministic VM | **consonance** | the VMM that runs deterministic guests (vmm-core + vmm-backend and the crates they compose) |
| the fuzzer (abstract; ~tasks 17/19) | **dissonance** | the corpus/fuzzer that hunts for divergence |
| `detharness` (crate) | **unison** | the determinism harness (replay-equivalence / `compare_runs` / `bisect_divergence`) |

Mnemonic to preserve in the rename: **consonance** = parts agreeing (the engine), **dissonance** =
the search for disagreement (the fuzzer), **unison** = the harness that checks they agree.

## Target source layout

Group crates under harmony-themed directories; **crate names stay technical** (only the grouping
dirs are themed, plus the one crate rename `detharness → unison`):

```
harmony/                      # repo root (GitHub repo + workspace; see "External actions")
  consonance/                 # the deterministic VM
    hypercall-proto/
    vmcall-transport/
    snapshot-store/
    vtime/
    vm-state/
    vmm-backend/
    vmm-core/                 # (once task 15 lands)
    lapic/                    # (once task 13 lands)
  unison/                     # the determinism harness (ex-detharness) — crate renamed `unison`
  dissonance/                 # corpus + fuzzer (once tasks 17/19 land): det-corpus/, fuzz/
  spikes/  docs/  guest/  tasks/  scripts/  .github/
```

The workspace `Cargo.toml` `members` changes from `["crates/*"]` to the grouped globs
(`["consonance/*", "unison/*", "dissonance/*"]` or equivalent). Crate **package names** are
unchanged **except** `detharness → unison`.

## Surface to touch (exhaustive — a half-renamed tree is the failure mode)

A tree-wide sweep. At minimum:

- **Move dirs**: `crates/*` → `consonance/*`; `crates/detharness` → `unison/` (and rename the
  package `detharness` → `unison`). Reserve `dissonance/` for the corpus/fuzzer crates.
- **Cargo**: workspace `members`/`default-members`; every crate's path-dep and `[dependencies]`
  entry that names `detharness`; `Cargo.lock`.
- **Rust imports**: every `use detharness::…` / `detharness::` path → `unison::` (≈ the harness's
  dependents: `vmm-core` dev-dep, `det-corpus` (task 17), and any test).
- **CI** (`.github/workflows/quality.yml`): the `-p <crate>` lists in the miri / public-api /
  coverage jobs; any path-scoped step; `.cargo/mutants.toml` exclude globs (`**/clock_proofs.rs`,
  `**/device_proofs.rs`, the `cfg(linux)` kvm regex — paths change under `consonance/`);
  `--ignore-filename-regex` in the coverage job (the `crates/vmm-backend/src/kvm…` path).
- **Public-API snapshots**: `tests/public-api.txt` per crate (the crate name appears in the
  snapshot — `detharness` → `unison`).
- **Scripts**: `scripts/push-docs-to-main.sh` allowlist regex (it excludes `crates/**`; that
  becomes `consonance/**` etc. — and crate code must **stay PR-gated**, so the new dirs are
  **excluded** from the docs allowlist exactly as `crates/` was); `scripts/agent-spawn.sh` /
  worktree paths if any embed `crates/`; `scripts/install-quality-tools.sh` / `.githooks/pre-push`
  `MIRI_CRATES`.
- **Box runner**: nothing path-specific should live on the box (CI checks out fresh), but verify
  `setup-ci-runner.sh` and `docs/BOX-PINNING.md` carry no stale crate paths.
- **Docs**: every reference to "hypervizor" → "harmony" and `detharness` → `unison` across
  `docs/`, `tasks/`, `README*`, `AGENTS.md`, `PLAN.md`, `RESEARCH.md`, `INTEGRATION.md`, the
  contract docs; the conceptual "the deterministic VM" / "the fuzzer" prose where the new proper
  nouns (consonance/dissonance) read better.
- **Skills**: `.claude/skills/*` (foreman/handoff/pr-review) reference `~/workspace/hypervizor`,
  `crates/*`, `detharness` — update paths + names.

## External actions (integrator/user, NOT the worker)

The worker renames **inside the repo** only. Flag these for the integrator to do out-of-band:

- **GitHub repo rename** `pH14/hypervizor → pH14/harmony` (GitHub keeps a redirect; update the
  remote URL, `REPO_URL` in `setup-ci-runner.sh`/`add-ci-runners.sh`, the runner registration).
- **Local workspace dir** `~/workspace/hypervizor → ~/workspace/harmony` (+ worktree siblings) and
  the `~/.claude/projects/...` path — user's machine, user's call.
- Whether the **crate package** `detharness` truly renames to `unison` on disk+`Cargo.toml`, or
  only the directory moves — recommend the full crate rename (it's a name, not just a location).

## Sequencing & determinism

- **Freeze window only** (see the directive at the top — planned, post-M1, queue-clear, ideally
  overnight). Land it as **one atomic PR** (or a tightly-ordered pair) when no other task
  PRs/branches are open — otherwise every open branch conflicts on every moved file. The foreman
  announces the window and pauses spawns.
- **Determinism-neutral (verify, don't assume).** Crate names/paths must not reach any **hashed**
  input: `contract_hash` is over the §6 canonical form (kernel-tag/cpuid/MSR tables — no crate
  names); `state_hash` is runtime architectural state. Grep the hash inputs to confirm no rename
  touches them. If a crate name *does* appear in a hashed/golden artifact, that's a finding.

## Acceptance gates

1. Full standard gate suite green **after** the rename: `build`/`nextest`/`clippy -D warnings`/
   `fmt`/`deny`, **plus** miri / coverage / mutants / kani / public-api — all jobs pass with the
   new paths/`-p` lists.
2. **No stragglers**: `git grep -nI 'hypervizor\|detharness\|crates/'` returns only intentional
   historical references (e.g. a CHANGELOG note, the GitHub-redirect mention) — zero live code,
   Cargo, CI, or import references to the old names/paths.
3. The CI runner runs the suite on the renamed tree (verify on the box / a CI run).
4. `IMPLEMENTATION.md` (or a short `docs/RENAME.md`) records the map, the external actions left for
   the integrator, and the determinism-neutrality check result.

## Non-goals

Any behavior change; new features; renaming the *concepts inside* crates (only the project/group
names + `detharness→unison`); the GitHub-repo / local-dir / remote renames (integrator). Do not
start until the freeze window is declared.
