# Task 19 — `fuzz/`: determinism fuzzer (C2 corpus)

Read `tasks/00-CONVENTIONS.md` first, then `docs/DETERMINISM-CORPUS.md` (§C2). Touch the
top-level `fuzz/` directory, **plus** the two CI-wiring files the fuzz-job gate requires:
`.github/workflows/quality.yml` (the nightly `fuzz` job) and `scripts/install-quality-tools.sh`
(`cargo-fuzz`). Those two are the *only* permitted edits outside `fuzz/` — they are the gate's
own wiring, not crate code.

## Environment

Fast tier: macOS **and** Linux, **nightly** toolchain (cargo-fuzz / libFuzzer require nightly —
use the pinned nightly from `.github/workflows/quality.yml`'s `miri` job) and the `cargo-fuzz`
binary (add it to `scripts/install-quality-tools.sh`). Does **not** require `/dev/kvm`. The
real-KVM tier is box-gated and **outlined only** here (it depends on `vmm-core`, which is
frontier); this task delivers the fast tier plus the `Arbitrary` input model the real-KVM tier
will reuse unchanged.

A top-level `fuzz/` is the standard cargo-fuzz layout: its `fuzz/Cargo.toml` carries an empty
`[workspace]` table, detaching it from the root workspace — so it owns its own deps and **does
not edit the root manifest** (conventions rule 1). Build deps: `libfuzzer-sys`, `arbitrary`
(whitelisted as a dev-dep), and path deps on the crates under test. `libfuzzer-sys` is outside
the whitelist — note it ask-by-comment in the PR (it is the standard cargo-fuzz harness crate).

## Context

C2 is the generated/fuzzed corpus. A fuzzer here is an **input generator that feeds the
oracles** — it defines no new property. The checks are the existing oracles: O1 determinism
(`unison::compare_runs` / `bisect_divergence`) and O3 seed-sensitivity (`det-corpus`, task
17). Two tiers, split because a KVM run is microseconds-of-ioctls, not the nanoseconds libFuzzer
wants:

- **Fast tier (this task):** in-process, runs anywhere, millions of cases.
- **Real-KVM tier (outlined, box):** `Arbitrary` VM input → `compare_runs` on the real `Vmm`,
  made viable by fast snapshot-reset (the one Nyx mechanic worth lifting — `RESEARCH.md:81`).

## The targets

Three libFuzzer targets in `fuzz/fuzz_targets/`:

1. **`decode_hypercall`** *(dep: `hypercall-proto` — merged; independently landable).* Fuzz the
   frame decoder: `decode(bytes)` must **never panic** on arbitrary input — the primary Tier-1
   target (`CODE-QUALITY.md`, "library code must never panic on untrusted input"). The round-trip
   is the **narrower** claim and must **not** be asserted byte-for-byte on every decodable `x`: a
   buffer can decode while carrying page-tail bytes after `payload_len`, or a header field whose
   raw value the public `encode_*` API can't reproduce — so `encode(decode(x)) == x` false-crashes
   on valid frames. Assert it only on the **canonical** subset (re-decode stability —
   `decode(encode(decode(x))) == decode(x)` — or a canonical-frame generator), never on arbitrary
   bytes.
2. **`snapshot_ops`** *(dep: `snapshot-store` — merged; independently landable).* The merged
   `snapshot-store` exposes **builder operations** (`begin_base`/`derive`/`write_page`/`seal`/
   `read_page`) — **not** a delta-byte-string parser — so fuzz an `Arbitrary`-generated
   **sequence of those public operations** (random page writes, derive chains, reads/seals):
   assert no panic and that the store's documented invariants hold (no OOB, COW/delta-resolution
   invariants, read-back consistency). Do **not** invent an uncontracted delta codec against a
   private surface — drive only the public API.
3. **`toy_determinism`** *(deps: `unison` + `det-corpus` — task 17; lands after 17).*
   `Arbitrary` → a generated toy program + seed → `det-corpus::check_determinism` (O1) over
   `ToyFactory` must return `passed: true`, and `check_seed_sensitivity` (O3) over a
   control-flow-stable RNG program must hold. A failure is a fuzzer find = a real determinism
   bug in the generated-program model or the harness.

Define the `Arbitrary` input model in `fuzz/` and use it in target 3, so the real-KVM tier
reuses it verbatim:

```rust
#[derive(arbitrary::Arbitrary, Debug)]
struct FuzzVmInput {
    seed: u64,
    program: GenProgram,            // instruction mix the guest runs (toy now; guest payload later)
    hypercall_script: Vec<HcResp>,  // deterministic host responses (entropy/block bytes)
    interrupt_schedule: Vec<u64>,   // V-time work-counts at which to inject the timer IRQ
}
```

For the toy tier, a thin adapter interprets `program` as a `ToyMachine` program and
`hypercall_script`/`interrupt_schedule` against the toy; for the real-KVM tier the same fields
map onto the VMM's hypercall responses and injection planner. One struct, two backends.

## Acceptance gates

Beyond the standard gates (`cargo +nightly build/clippy/fmt` on the `fuzz/` crate):

1. **Each target runs clean for a bounded time**: `cargo +nightly fuzz run <target> --
   -max_total_time=30` exits 0 (no crash/leak/timeout). Targets 1 & 2 need only merged crates;
   target 3 is added once task 17 merges (land 1 & 2 first if 17 is not yet in).
2. **Oracles have teeth** (smoke): a deliberately-broken assertion in `decode_hypercall` (e.g.
   asserting a wrong round-trip) makes the target crash — proving the check is real, then revert.
3. **Committed seed corpus**: `fuzz/corpus/<target>/` seeded from real inputs — valid encoded
   frames from `hypercall-proto`'s tests for `decode_hypercall`; the task-18 payload descriptors
   for `toy_determinism` once available. Document seed provenance in `fuzz/README`.
4. **Regression replay without nightly**: a normal **stable** `cargo test` target replays the
   committed corpus + any found-crash inputs through the same target functions (libFuzzer corpus
   files are plain byte slices) — so the default CI lane gates regressions without nightly. Every
   historical crash input is committed under `fuzz/corpus/<target>/` and must pass.
5. **CI wiring**: (a) the **stable regression replay** (gate 4) must run in the **default** CI lane
   — since `fuzz/` is a detached workspace (its own `Cargo.toml`), the existing `gates` job's root
   `cargo nextest run` will **not** discover it, so add a step that runs the replay explicitly via
   `cargo test --manifest-path fuzz/Cargo.toml` (stable toolchain, no nightly); (b) add a separate
   **nightly** `fuzz` job (a short `-max_total_time` `cargo fuzz` smoke per target, total ≤ ~2 min)
   to `.github/workflows/quality.yml`; (c) add `cargo-fuzz` to `scripts/install-quality-tools.sh`.
   Without (a), the claimed default-lane regression gate never executes.

## Non-goals

The real-KVM tier *implementation* (box + `vmm-core`, frontier — only the input model is
delivered here); coverage-guided exploration / scoring (that is the explorer, task 12);
long-running fuzz campaigns (CI runs short smokes; deep runs are an ops/box concern); hosting
the engine inside Nyx (see the `DETERMINISM-CORPUS.md` borrow table — nested virt degrades the
PMU/TSC the engine rests on).
