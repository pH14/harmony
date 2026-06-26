# Task 35 — kill the surviving mutants from the first full-tree mutation run

> The initial-release PR (harmony #1) ran `cargo mutants --in-diff` against an empty base, which made
> the diff the **entire tree** — the first time the whole codebase was mutation-tested at once (2424
> mutants). It surfaced **22 surviving (`MISSED`) mutants** plus **3 timeouts** that per-PR `--in-diff`
> never caught, because each line was only ever mutated against its own small diff. These are **latent
> test-tightness gaps in already-shipped, correct code** — the production logic is right; the tests
> just don't *pin* these exact boundaries/values. This task closes the gaps with exact-value tests.

Read `tasks/00-CONVENTIONS.md` (esp. the mutation-testing bar: tests must pin **exact** values/counts,
and stateful/codec logic wants a property or stateful test against an **independent** reference model,
not a mirror of the impl), `tasks/24-environment.md`, `tasks/26-pv-net.md`, and `mutants.toml` first.

## Principle

The code is **correct** — do **not** change production logic to "fix" a mutant. Add or strengthen
**tests** so each listed mutation is *detected* (the test fails when the operator/constant is changed).
If strengthening a test reveals that a surviving mutant is actually a **real bug** (the code does the
wrong thing), STOP and flag it in the PR rather than papering over it — but the expectation is these
are test gaps.

## The surviving mutants to kill

### `dissonance/pv-net` — untrusted-input parser/codec (highest priority; these decode wire bytes)
- `src/codec.rs:104` `decode_into` — `>`→`==` and `>`→`>=` (length/bounds check)
- `src/codec.rs:108` `decode_into` — `<=`→`>`
- `src/codec.rs:130` `decode_into` — `<=`→`>` and `>`→`>=`
- `src/codec.rs:156` `decode_into` — `||`→`&&`
- `src/codec.rs:169` `decode_into` — `||`→`&&` and `<=`→`>`
- `src/parse.rs:107` `Ipv4Dissection::conn` — `<=`→`>`
- `src/parse.rs:118` `endpoint_bytes` — return value replaced with `[0; 6]` and `[1; 6]` (constant-return)
- `src/parse.rs:131` `parse_ipv4` — `<`→`>`
- `src/parse.rs:181` `fnv1a64` — return value replaced with `1` (constant-return)
- `src/parse.rs:183` `fnv1a64` — `^=`→`|=` (the FNV mixing step)
- `src/switch.rs:201` `Switch::route_one` — `>`→`>=`
- `src/switch.rs:247` `Switch::throttle_blocks` — `-`→`+`
- `src/lib.rs:82` — `<<`→`>>` (a bit-shift)

### `dissonance/environment`
- `src/catalog.rs:178` `DecisionPoint::admits` — `<`→`<=` (boundary of the admissible range)
- `src/codec.rs:141` `read_answer` — `>`→`==` and `>`→`>=`
- `src/seeded.rs:67` `SeededEnv::supply_bytes` — `-`→`+`

### Timeouts — make them **deterministically caught**, not hang-caught
These mutants currently exceed the test timeout (~372 s) rather than failing fast — fragile and slow.
Add a **bounded** test that detects the changed behavior quickly (e.g. assert an exact result/length on
a small input that the mutated loop bound/accumulator would get wrong):
- `dissonance/environment/src/seeded.rs:65` `SeededEnv::supply_bytes` — `<`→`<=` and `<`→`==` (loop bound)
- `consonance/snapshot-store/src/lib.rs:521` `BuilderCore::seal` — `+=`→`*=` (an accumulator)

## How to verify (do this, don't eyeball it)

For each crate, run mutants scoped to the touched files and confirm every mutant above is now `caught`:
```sh
cargo mutants -p pv-net -p environment            # or scope by --file per mutants.toml
cargo mutants -p snapshot-store --file consonance/snapshot-store/src/lib.rs
```
Re-run until the listed mutants report **0 missed / 0 timeout**. Quote the before/after mutant counts in
the PR. (The per-PR CI `mutants` gate is `--in-diff`, so it will mutate your *new test lines*; that's
fine — the point is the targeted re-run above shows the previously-surviving mutants are dead.)

## Acceptance gates

1. **All 22 `MISSED` mutants killed** — the targeted `cargo mutants` re-run reports them `caught`.
2. **The 3 timeouts caught deterministically** — a bounded test fails fast on the mutation (no reliance
   on the 372 s hang); the targeted re-run shows them `caught`, not `timeout`.
3. **No production-logic change** — the diff is tests (+ test helpers) only, unless a real bug was found
   and flagged. Determinism preserved (no `HashMap`/wall-clock/unseeded-rng in new test paths that reach
   hashes/output; pv-net/environment codecs stay byte-deterministic).
4. **No regression** — `cargo nextest run --all-features` green for both crates; `cargo clippy
   --all-features --all-targets -- -D warnings` and `cargo fmt --check` clean. Coverage does not drop.
5. **New tests pin exact values** — comparison/constant/shift mutants demand exact-count/exact-byte
   assertions (the codec/parse tests should assert the precise decoded struct / exact bytes / exact
   hash, and the boundary tests should assert behavior *at* and *either side of* the boundary).

## Non-goals

Changing any production code (the logic is correct); other crates beyond the three named; new features;
re-running the full 2424-mutant tree in CI (that stays a one-off — normal PRs use `--in-diff`). Build on
the existing `pv-net`/`environment` test suites — strengthen them, don't rewrite.
