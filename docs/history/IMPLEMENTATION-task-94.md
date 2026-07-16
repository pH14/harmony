# Task 94 — rename the two loops: **Modulation** (inner) / **Progression** (outer)

A pure, behavior-neutral rename. Three competing vocabularies for the two exploration
loops — `docs/DISSONANCE.md`'s *Variation/Theme*, the explorer code's *Timeline/Multiverse*
(`timeline()`/`multiverse_step()`), and the already-post-rename *Modulation/Progression* in
`docs/EXPLORATION.md`/`docs/RESOLUTION.md` — are unified to the integrator's ruling:

- **Modulation** — inner loop: one run under one environment (was *Variation* / *Timeline*).
- **Progression** — outer loop: the search across runs (was *Theme* / *Multiverse*).

## The rule I applied (loop proper noun vs. temporal-axis common noun)

The rename targets the **loop names** (proper nouns). It deliberately does **not** touch the
lowercase term of art **"timeline"** meaning *a run's `Moment` axis / an ordered sequence* — most
visibly **"timeline admission"** (spine invariant 2: admit an exemplar at every novel `(cell,
Moment)` along a run), plus "along-timeline features", "sealable/whole timeline", "ordered
timeline", "mid-timeline". This split is not my invention: `docs/EXPLORATION.md` — the
integrator's own post-rename doc — uses **Modulation/Progression** for the loops while keeping
**"timeline admission"** and **"along-timeline cell keys"** verbatim (lines 118, 263). I mirrored
it exactly. Likewise "multiverse" as the **VMM branch-tree** primitive (task 48/49, `consonance/`)
and UI "timeline"/"theme" are different concepts and stay.

Concretely:

| Renamed (loop proper noun) | Kept (distinct concept) |
|---|---|
| `Explorer::timeline()` → `modulation()` | `timeline admission`, `along-timeline`, `timeline features` (run's Moment axis) |
| `Explorer::multiverse_step()` → `progression_step()` | `ordered timeline` / `genesis timeline` (the `environment` Moment axis) |
| "one Timeline" / "the Multiverse" (a run / the search) | "multiverse" = VMM branch-tree (`consonance/vmm-*`, task 48/49) |
| "Theme" (outer loop, `DISSONANCE`/`environment`) | UI "event timeline" (`consonance/telemetry`), "Theme A/B" section labels (task06) |
| "Variation" (inner loop, `DISSONANCE`/`control-proto` "variation unit") | Antithesis "multiverse debugging" (external product term, `RESOLUTION`) |

## Determinism-neutral by construction (verified, not assumed)

There are **no** `Timeline`/`Multiverse` types, enum variants, or serialized/`serde` fields — the
public surface only ever had the two **method names** plus doc comments (see `public-api.txt`).
Method names never reach wire bytes, hashes, or goldens:

- **Wire (`control-proto`):** the verb set is `hello`/`snapshot`/`drop`/`branch`/`replay`/`run`/
  `perturb`/`hash` — no loop-named request/reply variant. Grepped the wire types: none carry a
  renamed word.
- **Hashes:** `Bug.fingerprint` is `sha2` of the `StopReason` (`Deadline`/`Quiescent`/`Crash`/
  `Decision`/`Assertion`/`SnapshotPoint`) — no loop name.
- **Goldens:** the **only** fixture changed is `dissonance/explorer/tests/public-api.txt`
  (regenerated with `cargo public-api` on the pinned nightly — exactly the two method lines,
  re-sorted, no other surface drift). No `live_*` / corpus golden changed (`git diff --name-only`
  over the branch shows `public-api.txt` as the sole `.txt`/fixture).

Empirical confirmation (gate 3, zero behavior change):
- `explorer::behavior_equiv fifty_campaigns_are_byte_identical_across_the_refactor` — **pass**
- `explorer::determinism same_seed_yields_identical_campaign` — **pass**
- `conductor::…branch_run_hash_is_deterministic_and_replay_reproduces_capture` — **pass**

## Surface touched

- **`dissonance/explorer`** — `timeline()`→`modulation()`, `multiverse_step()`→`progression_step()`
  across `lib.rs`/`engine.rs`/`seam.rs`/`error.rs`/`adapter.rs`, all tests, `IMPLEMENTATION.md`,
  and the `public-api.txt` golden. **`tests/reference/mod.rs` is deliberately left untouched** — it
  is the task-12 engine frozen verbatim for the behavior-equivalence gate (see *Exclusion-list
  compliance* below).
- **58/60 consumers & catalog** — `conductor` ("the multiverse"→"the progression"; "current
  Timeline"→"Modulation"), `control-proto` ("the variation unit"→"the modulation unit"),
  `environment` (doc comments: "Theme"→"Progression"; kept "ordered timeline").
- **Docs** — `DISSONANCE.md` (two-loops section + table + "Progression is agnostic" section +
  **naming-history footnote**), `dissonance/README.md`.
- **Task specs** — one-line **historical note** atop `tasks/12-explorer.md` (task-90 precedent);
  `tasks/60` dropped its now-obsolete "pre-task-94 naming" hedge. Historical specs 12/24/25/45/93
  keep their original vocabulary by design.

## Gate 1 — the surviving `git grep -niE 'variation|theme|timeline|multiverse'` over `docs/ dissonance/ consonance/`

Every remaining hit is one of the three allowed categories, verified by eye:

**(a) The `DISSONANCE.md` naming-history footnote** — `docs/DISSONANCE.md` §"The two loops"
(the old→new decoder) and its single "ordered timeline" (Moment axis).

**(b) Naming-history mappings / roadmap records** (same role as the footnote, must retain
old→new to be meaningful): `docs/EXPLORATION.md:18,20` and `docs/RESOLUTION.md:13`
("formerly Theme/Variation…"); the task-94 rows in `docs/REVIEW-2026-07.md:126` and
`docs/ROADMAP.md:50`.

**(c) Historical / frozen records** (a record, not a lie to maintain):
`dissonance/explorer/tests/reference/mod.rs` (`fn timeline`/`fn multiverse_step`, "pre-refactor
Timeline" — the task-12 engine frozen verbatim, the behavior-equivalence golden; kept exactly as
the pre-rename code, see *Exclusion-list compliance*); `docs/history/IMPLEMENTATION-task-93.md:19`
("300 Multiverse steps" — task 93's run); `docs/history/IMPLEMENTATION-task06.md:124,127,144`
("structural themes", "Theme A/B" section labels); the `tasks/12` historical note.

**(d) Incidental — unrelated meaning:**
- **Temporal-axis term of art** (a run's `Moment` sequence, kept per `EXPLORATION.md`):
  "timeline admission" / "along-timeline" / "timeline features" / "sealable/whole timeline" /
  "mid-timeline" across `dissonance/explorer/{src,tests,IMPLEMENTATION.md}`; "ordered timeline" /
  "genesis timeline" / "host(-fault) timeline" across `dissonance/environment`.
- **VMM branch-tree "multiverse"** (task 48/49, the hypervisor's branching primitive — a
  different concept from the search loop): `consonance/vmm-backend/{IMPLEMENTATION.md,
  run_until.rs}`, `consonance/vmm-core/{IMPLEMENTATION.md, tests/live_branching_demo.rs}`.
- **Restore/retired-work "timeline"** (temporal axis): `consonance/vmm-core/{src/vmm.rs, tests/*,
  IMPLEMENTATION.md}`, `consonance/vm-state/*`, `docs/INTEGRATION.md:155`.
- **UI "event timeline"** (`consonance/telemetry/{assets,src}`); **debugger timeline UI** and
  Antithesis **"multiverse debugging"** product term (`docs/RESOLUTION.md`).

**This document itself** (`docs/history/IMPLEMENTATION-task-94.md`) is category (b)/(c): the task-94
record and old→new decoder, so it necessarily names both vocabularies throughout — the same way
`IMPLEMENTATION-task-93.md` and the DISSONANCE footnote do.

`tasks/` is outside the literal gate-1 path but I swept it to the same standard; the only
surviving loop-name uses there are historical specs (kept by design), the task-94 spec itself,
and forward decode-notes in future specs (64/66/67) that already point at this rename.

## Exclusion-list compliance

Three frozen/hashed surfaces were kept out of the rename:

- **Hashed strings — untouched.** No renamed identifier reaches a hash, wire byte, or golden
  (see *Determinism-neutral by construction*): `Bug.fingerprint` hashes the `StopReason` (no loop
  name), the `control-proto` verb set has no loop-named variant, and the `sha2`/`blake3` inputs in
  the touched crates are unchanged. The only golden edited is `public-api.txt` (the two method
  signatures), which is *not* a hashed run artifact.
- **Frozen reference model — untouched.** `dissonance/explorer/tests/reference/mod.rs` is the
  task-12 engine *frozen verbatim* — the golden the behavior-equivalence gate diffs the refactored
  engine against. Its internal `timeline`/`multiverse_step` names are part of that frozen record,
  so it is byte-identical to `main` (`git diff main -- …/reference/mod.rs` is empty). The gate still
  passes (`fifty_campaigns_are_byte_identical_across_the_refactor`): the reference's internal method
  names never enter the comparison, which is over run outputs.
- **History docs — untouched.** `docs/history/IMPLEMENTATION-task06.md` and
  `docs/history/IMPLEMENTATION-task-93.md` keep their original wording; their "Theme A/B" / "Multiverse
  steps" are historical records (and, for task06, unrelated section labels).

## Deviations considered and rejected

- **Renaming "timeline admission" → "modulation admission".** Rejected: `EXPLORATION.md`
  (post-rename, integrator-authored) keeps "timeline admission"; future specs 64/67/74 use it;
  it names the *Moment-axis trajectory*, which survives the loop rename. Renaming it would fork
  the vocabulary the spine contract already froze.
- **Renaming the VMM "multiverse" in `consonance/`.** Rejected: it is the branch-tree primitive
  (task 48/49), a substrate concept distinct from dissonance's search loop, and outside this
  task's surface. Left as incidental and listed above.
- **Renaming the frozen reference model's methods** (`tests/reference/mod.rs`). Initially applied,
  then **reverted**: it is the pre-refactor engine frozen verbatim, and its `timeline`/
  `multiverse_step` names are part of that record — renaming it would make it no longer verbatim.
  Left byte-identical to `main`; classified under gate-1 category (c).
- **Hand-editing `public-api.txt`.** Rejected in favor of regenerating with the real
  `cargo public-api -sss` (pinned nightly) so the sort is authoritative, not guessed.

## Known limitations / integrator notes

- **Naming collision to be aware of (not introduced here):** `DISSONANCE.md`'s `HostFault`
  carries "CPU modulation" (clock-rate modulation, `SetClockRate`). That is unrelated to the
  inner loop **Modulation** and is not a grep target; flagged only so a future reader does not
  conflate them.
- `REVIEW-2026-07.md` / `ROADMAP.md` task-94 rows are left phrased as the rename mapping; marking
  the task *done* in the roadmap is the foreman's bookkeeping, not a rename edit.
- No root files touched (rule 1); `explorer` is already in the `public-api` CI job's `-p` list.

## Gates

`cargo build` / `cargo nextest run` (233 passed across explorer+environment+control-proto+
conductor) / `cargo clippy -D warnings` / `cargo fmt --check` / `cargo deny check`
(advisories+bans+licenses+sources ok) — all green. `public-api.txt` regenerated and byte-matches.
