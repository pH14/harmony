# Task 105 — The vocabulary rename sweep (GLOSSARY ratification in code)

**Bead:** `hm-u7q` (P1). **The binding authority is `docs/GLOSSARY.md`** — its Kills /
Renames tables and the consonance addendum, as merged (PR #103 head). This spec does not
restate the slate; where this file and GLOSSARY disagree, GLOSSARY wins. Read its
"Required separations" and the addendum's `unison` rows before touching anything.

## Scope (from the GLOSSARY tables, verbatim authority)

- Crate renames: `dissonance/conductor` → `campaign-runner`; `dissonance/link` →
  `sdk-events` (and every path/Cargo.toml/import/CI reference).
- Type renames: `explorer::Environment` → `Reproducer` (the `environment::Environment`
  TRAIT keeps its name — do not rebuild the collision one level down; `EnvSpec` stays);
  `VTime`/`Vtime` → `Moment` (point) / `Span` (duration) — each use AUDITED into one or
  the other, not blind-replaced; `unison::Machine`/`MachineFactory`/`MachineError` →
  `Subject`/`SubjectFactory`/`SubjectError` — **the spine's `Machine` trait keeps its
  name** (the GLOSSARY blessed it; two different traits, only unison's renames).
- Anything else the GLOSSARY tables rule that a mechanical pass surfaces: follow the
  table, and if a case is genuinely ambiguous, STOP and put it in the PR description as
  a `[question]` — never improvise a name.

## Landmines (each is a binding constraint, most are review history)

1. **Wire and serialized-artifact neutrality is ABSOLUTE.** Rust identifier renames must
   not change any serialized byte: serde field/variant names that ride JSON artifacts
   (ExplorationLog, manifests, campaign logs), the versioned binary guest wire and
   reproducer blobs, journal/RunTrace encodings, hash inputs. Where a struct/field
   rename would leak into serialization, pin the old name (`#[serde(rename = ...)]`)
   or leave the field name alone — GLOSSARY's own rule: wire vocabulary changes only
   with a versioned format. **Gate: golden fixtures for every serialized artifact class,
   byte-identical before/after** (add goldens first if a class lacks one).
2. **Hash-neutrality proof.** Same-seed portable campaign runs (game + bench + planted)
   produce bit-identical hashes/logs pre- and post-rename. This is cheap (the suites
   already pin exact hashes) — state it in the PR, don't just imply it.
3. **CI references rename in lockstep.** The nightly Miri conductor step (just merged,
   PR #105) hard-references `-p conductor` AND the test path
   `mock::tests::mock_vmm_composes_maps_memory_and_ticks_per_exit` in its vacuity
   guard; quality workflows reference crate names too. Sweep `.github/workflows/*` in
   the same commits. The guards themselves prove the lockstep: a missed reference fails
   the step loudly (that is what they are for — do not weaken them to pass).
4. **The spike branch (PR #98) must not be broken.** `spike/nested-x86` is open and
   mid-re-certification. It barely touches crates, but check its `SPIKE(nested-x86):`
   marked diffs for references to renamed items; if any exist, note them in the PR
   description for the foreman (who owns the eventual rebase) — do NOT push to the
   spike branch.
5. **Docs sweep is included but subordinate**: rename in docs/ where they refer to the
   CODE artifacts (crate paths, type names). Do not rewrite prose vocabulary beyond
   that — the docs' conceptual language was already reconciled by the strategy slate.
6. **History hygiene**: use `git mv` for crate directories so history follows.

## Gates

- Full workspace: `cargo build --workspace`, `cargo nextest run --workspace`
  (+ guest workspaces: play-agent, flow-agent), `cargo clippy -D warnings` on host AND
  `x86_64-unknown-linux-gnu`, `fmt`, `deny`, workspace check on the linux target.
- Public-api snapshots: regenerate for renamed crates (expected: renames only — the
  snapshot diff IS the rename audit; include it in the PR description), unchanged for
  everything else.
- Golden-fixture byte-identity (landmine 1) + exact-hash test suite green (landmine 2).
- The full conductor(-now-campaign-runner) Miri suite green under the renamed paths
  (the PR #105 command with the new crate name), externally timed — it should still be
  ~12 min; the nightly.yml step and its guards updated in lockstep.
- No box gate (the re-cert window owns the box); nothing here needs one — this PR must
  be provably behavior-neutral by construction.

Done = one PR, mechanical-and-audited, with the GLOSSARY table as its checklist in the
description, every landmine addressed explicitly, and gates green. If the diff grows
past reviewability, split by crate along the GLOSSARY's own table rows and say so.
