# Code-quality tooling — backlog & rationale

Notes on tightening code quality beyond the current gates. **Status today:** every crate
runs `build`/`test`/`clippy -D warnings`/`fmt` (`tasks/00-CONVENTIONS.md`), `proptest` is
adopted in all four crates (≥256 cases), clippy is `all = deny`, and `vtime` additionally
denies float arithmetic. This doc is the plan for the next layer.

These are **not yet wired into CI** — they're a prioritized backlog. CI itself is a frontier
concern (the KVM/PMU gates only run on the bare-metal box; the pure-logic crates run
anywhere). Tools below are tagged by where they can run:

- 🟢 **portable** — runs on macOS + Linux, on the current pure-logic crates, no special HW.
- 🟡 **nightly** — needs a nightly toolchain or a separate tool toolchain.
- 🔴 **box-gated** — only meaningful once `vmm-core` exists and/or needs `/dev/kvm`.

Guiding principle: **determinism is the product, so the highest-value tools are the ones
that mechanically enforce determinism and prove the gates have teeth.** A passing test that
asserts nothing, or a `HashMap` iteration that leaks order into a hash, is exactly the class
of defect this project cannot tolerate. Prioritized accordingly.

---

## Tier 0 — adopt now (cheap, high-leverage, on existing crates)

### `clippy.toml` determinism lints — *the most project-specific win* 🟢
Convention rule #4 (no `HashMap`/`HashSet` iteration into outputs, no float in state, no
wall-clock, no unseeded `rand`) is currently enforced **by review**. Make it mechanical with
a workspace `clippy.toml`:

```toml
# clippy.toml
disallowed-methods = [
  { path = "std::time::Instant::now",       reason = "wall-clock leaks nondeterminism; use vtime" },
  { path = "std::time::SystemTime::now",    reason = "wall-clock leaks nondeterminism" },
  { path = "rand::thread_rng",              reason = "unseeded RNG; thread a seed explicitly" },
  { path = "rand::random",                  reason = "unseeded RNG" },
]
disallowed-types = [
  { path = "std::collections::HashMap",     reason = "iteration order can reach a hash/byte; use BTreeMap or sort" },
  { path = "std::collections::HashSet",     reason = "iteration order can reach a hash/byte; use BTreeSet or sort" },
]
```
Note: `HashMap` is legitimately used *internally* for lookup-only memoization (e.g.
`snapshot-store`'s per-layer index, where the value never reaches an output). Allow those by
`#[allow(clippy::disallowed_types)]` with a `// not order-observable:` justification — which
makes every such use a reviewed, annotated exception rather than an invisible default. This
single config turns the project's central invariant into a compiler error.

### `cargo-nextest` 🟢
Faster, better-isolated test runner. The isolation matters *here specifically*: nextest runs
each test in its own process, so a test that accidentally relies on global/static mutable
state (a determinism smell) fails in isolation instead of passing by luck of ordering. Drop-in
replacement for `cargo test` in the gates; pairs with llvm-cov below.

### `cargo-llvm-cov` 🟢  *(requested)*
Source-based line/region coverage. Add a coverage gate to each crate and a workspace rollup.
Recommendation: enforce a **region-coverage floor** (start at the current measured number,
ratchet up; don't pick a round number blindly). Run as `cargo llvm-cov nextest` so coverage
and the fast runner compose. Export `lcov` for a badge/CI artifact. Caveat: coverage measures
*reachability, not assertion strength* — which is why mutation testing below is its
indispensable complement, not a duplicate.

### `cargo-mutants` 🟢  *(requested)*
Mutation testing: mutates the source (flip `<` to `<=`, replace a return with `Default`, etc.)
and checks whether the test suite *notices*. A surviving mutant = a line your tests execute
but don't actually constrain. For a **gate-first project this is the highest-value tool in the
list**: it's the thing that proves the determinism gates have teeth rather than just green
checkmarks. Best targets first: `vtime` (the saturating arithmetic and planner state machine),
`snapshot-store` (CoW resolution / dedup logic), `hypercall-proto` (frame decode). Run it in
CI on changed files (full-tree mutation is slow); treat surviving mutants as review items, not
necessarily hard failures, until a baseline is established.

#### Status (quality-c) — *adopted*
`.cargo/mutants.toml` (the path cargo-mutants auto-discovers, so the bare gate command needs no
flag) configures the tool: nextest as the oracle, `--all-features`, a generous timeout
multiplier, `main.rs` excluded as demo glue. Approach: **clean the highest-value crate
to zero, gate everything else on the diff going forward.**

- **`unison`** — ✅ **mutation-clean.** `cargo mutants -p unison` reports zero surviving
  (un-caught) mutants. The gaps it found were all *exact-count* assertions the suite lacked
  (`checkpoints_compared` / `runs_executed` counters mutated to stay at 0, the `if lo > 0`
  start-of-time guard, and the uppercase-hex deserialize arm); killed by
  `consonance/unison/tests/mutation_kills.rs`. A few loop-condition mutants are caught by
  timeout (a non-terminating loop has no other tell) and one (`(hi<<4) | lo` → `^`) is a
  documented equivalent mutant — see that crate's `IMPLEMENTATION.md` "Mutation testing".
- **`vtime` / `snapshot-store` / `hypercall-proto`** — not yet swept to zero; they are guarded
  going forward by the **`--in-diff` CI gate** (below). Full-tree cleanup is follow-up work.

The CI `mutants` job (`.github/workflows/quality.yml`) is now **gating** (no
`continue-on-error`) and runs `cargo mutants --in-diff` against the PR diff, so any PR that adds
under-constrained logic on a changed line fails — regardless of which crate it touches.

### `cargo-deny` 🟢
Enforces the `tasks/00-CONVENTIONS.md` **dependency whitelist mechanically** (`bans` section),
plus RustSec advisories, license policy, and duplicate-version detection. Given the project
already maintains an explicit allowlist by hand, `deny.toml` is the natural automation —
"ask-by-comment if you need a new dep" becomes a failing check the PR author sees first.

---

## Tier 1 — fuzzing & deeper property testing (the parsers and state machines)

### `cargo-fuzz` + `arbitrary` 🟡
Coverage-guided fuzzing. There is one *perfect* first target: **`hypercall-proto`'s frame
decoder** — it parses untrusted bytes (a hostile host/guest), and the convention is "library
code must never panic on untrusted input." A fuzz target over `decode()` that asserts
no-panic + round-trip (`encode(decode(x))` stable) is exactly the discipline a wire-format
boundary wants. Second target: `snapshot-store` delta application. Commit the seed corpus.

### `proptest-state-machine` (stateful proptest) 🟢
The current proptests are mostly value-level. The two crates with real *state machines* —
`snapshot-store` (`Store` across sequences of begin/write/seal/release/gc) and
`hypercall-proto`'s `Dispatcher` (register/dispatch/save/restore) — benefit from
**model-based** testing: drive a random sequence of operations against both the real
implementation and a simple reference model, assert they agree. `unison` already embodies
this philosophy (it *is* an oracle harness); extend the pattern into the stores.

### `insta` 🟢
Snapshot testing for canonical encodings: the hypercall wire frames, the `vm_state` blob codec
(task 09 when it lands), the CPU/MSR contract artifacts (`docs/cpu-msr-contract.toml` is
already a golden-style artifact), and `unison` state-hash outputs. Makes "the encoding
changed" a reviewable diff instead of a silent break. Complements the existing `guest/golden/`
approach.

---

## Tier 2 — unsafe & formal correctness (mostly lands with `vmm-core`)

### `cargo-kani` (bounded model checking) 🟡 — *usable now on `vtime`*
Unlike the rest of this tier, Kani pays off **today**: `vtime`'s saturating `u128` arithmetic
and `work_for_vns` ceil-division are small, pure, integer functions whose "never panics / law
holds for *all* inputs" claims are exactly what bounded model checking proves — stronger than
proptest's sampling. Good first proof harnesses: "`vns`/`tsc` never panic and saturate
correctly for all `(config, work)`" and "the `vns(work_for_vns(t)) >= t` round-trip law holds
in the reachable regime." Later: the snapshot delta-resolution invariants.

### Miri 🟡
UB detector for `unsafe`. Mostly relevant to `vmm-core` and `snapshot-store`'s `mmap` path
(Miri can't run real `mmap`/KVM ioctls, but it can run the pure-logic crates end-to-end to
catch uninit reads, alignment, provenance, and strict-aliasing bugs, and it flags some sources
of nondeterminism). Run the portable crates' test suites under Miri in CI; the `no_std` guest
client is a good Miri candidate too.

### `-Zsanitizer=address,thread` 🔴
For `vmm-core`'s FFI/`unsafe` against KVM. ASan for memory errors; TSan for the **host-side**
concurrency only (see loom note). Box-gated — meaningful once there's real unsafe systems code.

### `loom` — *mostly N/A; note for honesty* 🔴
Exhaustive concurrency-permutation testing. The **guest is single-vCPU by design**, so loom
buys nothing there — and saying so is itself a design check. It becomes relevant only for
**host-side** shared state: the explorer driving N `vmm-core` processes, or any shared
host data structure. Apply it narrowly there if/when such state appears; do **not** reach for
it on the deterministic guest path.

---

## Tier 3 — API-contract & hygiene

### `cargo-public-api` / `cargo-semver-checks` 🟢 — *adopted (cargo-public-api)*
The delegated crates' public APIs are explicitly **frozen contracts** (`00-CONVENTIONS.md`
rule 3, `INTEGRATION.md` "frozen" seams). `cargo-public-api` checks in a textual snapshot of
the public surface so any change shows up as a reviewable diff; `cargo-semver-checks` flags
accidental breaking changes. Directly operationalizes the "public API is a contract" rule.

See **"Public-API snapshots"** below for the wired-in implementation (quality-d).
`cargo-semver-checks` remains future backlog.

### `cargo-machete` (or `cargo-udeps` 🟡) 🟢
Detect unused dependencies — keeps the tight whitelist honest as code churns. `machete` is
stable-toolchain; `udeps` is more thorough but nightly.

### `iai-callgrind` — *deterministic benchmarking* 🟢
A clever fit: instruction-count-based benchmarks (via cachegrind) give **stable,
machine-independent** numbers — no wall-clock noise — which suits a determinism project and
makes perf regressions diff-able in CI. This is also the natural home for the **throughput
budget the plan currently lacks** (single-step cost per injection, snapshot-resolve cost per
page): turn `snapshot-store/tests/bench.rs` into tracked iai benchmarks rather than ad-hoc
timing. (`criterion` is the wall-clock alternative — use only for things iai can't model.)

---

## Deliberately skipped (and why)

- **`cargo-vet`** — supply-chain trust-auditing of every dependency. Overkill given the tiny,
  hand-curated whitelist; `cargo-deny`'s advisory + license checks cover the real risk.
- **`quickcheck`** — `proptest` is already adopted and strictly more capable (shrinking,
  regression files); don't run two property frameworks.
- **`tarpaulin`** — `cargo-llvm-cov` (source-based) is more accurate than tarpaulin's
  ptrace/instrumentation approach on modern toolchains; pick one (llvm-cov).

---

## Coverage baseline (2026-06-16)

Measured with `cargo llvm-cov nextest --all-features` on this workspace (123 tests,
2 skipped). **Region coverage** is the gated metric; the CI `coverage` job enforces a
floor of **93%** (`--fail-under-regions 93`) — `floor(93.86%)`, the measured workspace
number rounded down, no margin padding. Ratchet the floor up as coverage improves; never
round to a clean number. Reproduce locally with `scripts/coverage.sh`.

| Scope                  | Regions | Missed | Region cover |
| ---------------------- | ------: | -----: | -----------: |
| unison             |    1278 |     33 |       97.42% |
| hypercall-proto        |     984 |    158 |       83.94% |
| snapshot-store         |     726 |     40 |       94.49% |
| vtime                  |     857 |      5 |       99.42% |
| **workspace (TOTAL)**  |    3845 |    236 |   **93.86%** |

The floor tracks the *workspace* number. `hypercall-proto` (decode-heavy, many unreached
malformed-frame branches) is the main drag; raising its coverage is organic test work, out
of scope for this gating task.

### Ratchet: 93% → 94.5% (2026-07-05, issue #69)

A CI compile break (`Step::SdkStop`, #63→#68) had failed the coverage job *before it could
measure* for several merges, letting `dissonance/campaign-runner`'s `record.rs`/`campaign.rs`/
`lib.rs`/`main.rs` accumulate thin coverage undetected (68–83% region, `main.rs` 0%). #71
added targeted gate-branch and CLI-dispatch tests to those four files (see
`dissonance/campaign-runner/IMPLEMENTATION.md`'s "Coverage recovery" section for what was added),
and split `main.rs`'s Linux-only `mod boxrun` (needs real `/dev/kvm` + patched KVM + a built
guest image — uncoverable by this job, same reasoning as the `kvm.rs`/`patched_kvm.rs`/
`pmu_sys.rs`/`work_perf.rs` exclusions above) into its own `src/boxrun.rs`, added to
`--ignore-filename-regex`. Measured workspace region total after: **94.76%**
(`cargo llvm-cov nextest --all-features`, on the determinism box — Linux, so every
`cfg(target_os = "linux")` line compiles and counts). Floor moved to **94.5%** — a hair
below, not the measured number itself (leaves room for ordinary cross-run noise without
inviting the floor to silently drift back down). Reproduce on the box (a Mac-local run
understates anything `cfg(target_os = "linux")`, since it doesn't even compile there).

---

## Public-API snapshots (2026-06-17)

Each of the four crates (`hypercall-proto`, `snapshot-store`, `unison`, `vtime`) carries a
committed snapshot of its public surface at `tests/public-api.txt` and a guard test at
`tests/public_api.rs`. The test shells out to `cargo public-api` on a **pinned nightly**,
regenerates the surface, and asserts it byte-matches the committed snapshot — so any drift in a
frozen contract is a failing test and a reviewable diff. The CI `public-api` job (gating, no
`continue-on-error`) runs these tests on every PR.

- **Pinned nightly:** `nightly-2026-06-16`. `cargo public-api` needs rustdoc-JSON, which is
  nightly-only; pinning keeps the output reproducible. The same constant lives in each
  `tests/public_api.rs` (`PINNED_NIGHTLY`) and the workflow's `PINNED_NIGHTLY` env — keep all
  three in sync when bumping. Install with `rustup toolchain install nightly-2026-06-16`.
- **Surface flags:** generated with `-sss` (omit blanket, auto-trait, and auto-derived impls)
  so the snapshot is the genuine hand-written API, not toolchain-version-dependent auto-impl
  noise. Default features only (the host-side build vmm-core integrates against).
- **Refresh after an intentional, reviewed API change:**
  `UPDATE_PUBLIC_API=1 cargo test -p <crate> --test public_api`, then review the diff.
- **No new crate dependencies:** the guard invokes the installed `cargo-public-api` *binary*
  (Convention rule-5 tool exemption) rather than adding the `public-api`/`rustdoc-json` library
  crates. On a box lacking the nightly or the tool, the test **skips loudly** (keeps a plain
  stable-only `cargo nextest` green); CI installs both, so the gate runs for real there.

## Suggested adoption order

1. **`clippy.toml` determinism lints** + **`cargo-deny`** — mechanize the conventions that are
   currently review-only. Cheapest, highest-leverage, no test changes.
2. **`cargo-nextest` + `cargo-llvm-cov`** with a ratcheting coverage floor — visibility.
3. **`cargo-mutants`** on the four crates — proves the gates and the new coverage have teeth.
4. **`cargo-fuzz` on `hypercall-proto::decode`** + **stateful proptest** on the two stores.
5. **Kani harnesses on `vtime`** arithmetic — punch above proptest where the surface is small.
6. The rest (`insta`, `cargo-public-api`, `iai-callgrind`, Miri) as the surfaces they guard
   (vm_state codec, frozen APIs, perf budget, unsafe) actually materialize.
