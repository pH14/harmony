# Prescriptive V-time implementation status

Branch: `claude/consonance-virtual-time-6kvrz6`

This is the live evidence ledger for `docs/PRESCRIPTIVE-VTIME.md`. A criterion is
`PASS` only when its positive oracle and every applicable anti-vacuity oracle have
both run. `FAIL` includes work not started. `BLOCKED` names an environmental
dependency rather than silently weakening the criterion.

## Recorded decisions

1. **M0 contract durations are explicit placeholders.** The four exported
   `PLACEHOLDER_*_VNS` constants are intentionally named non-normative values.
   M1 must replace them with the values recorded in the arm64 determinism contract;
   no production composition may silently accept the defaults.
2. **A schedule records when a deadline became eligible.** A timer armed between
   exits records `armed_for_event`. Without this, an independent checker would
   incorrectly demand that a newly armed, already-due timer had fired at an event
   that occurred before the timer existed. This was found by the first dedicated
   WFI-at-deadline oracle run (9 passed, 1 failed), then corrected before any M0
   pass was claimed.
3. **Delivery has an explicit fabric callback.** `PrescriptiveRunLoop` invokes the
   caller's delivery seam for every due identity before hashing/logging the exit.
   The `MockBackend` oracle records all six deliveries from the dedicated workload;
   the normalized log is not standing in for actual delivery.
4. **Raw logs are substrate-local; normalized logs are the comparator input.** The
   raw log retains the backend reason and full debug rendering. Cross-substrate
   claims compare only the normalized event stream.

## M0 — prescriptive advancement in pure logic

### Build criteria

- **PASS — assigned-at-exit run loop in `vmm-core`.** `PrescriptiveRunLoop` drives
  `Backend::run`, classifies the exit through the vendor seam, keeps `VClock` at
  work zero, advances only through `VClock::advance_idle`, raises due interrupts,
  and never calls `run_until`.
- **PASS — raw and normalized logs.** Each normalized event carries its index,
  class plus a domain-separated complete-payload digest, post-advance V-ns,
  ordered deliveries, and checkpoint/final state hash.
- **PASS — independent delivery checker.** `check_delivery_placement` derives the
  expected placement from the immutable schedule and normalized post-advance V-ns;
  it does not call or share `TimerQueue` logic.
- **PASS — prescriptive V-time and entropy in `state_blob`.** The committed test
  constructs `VtimeWiring::new_prescriptive`, advances at work zero, proves the
  `VTIM` chunk exists, and independently perturbs assigned V-time and entropy to
  show each changes `state_hash`.

### Passes-when criteria

- **PASS — monotonicity property.** 256 generated saturating increment scripts.
- **PASS — generated schedule placement.** 256 generated schedules; every deadline
  is checked at its first eligible exit.
- **PASS — masked / WFI-overlap / simultaneous / reassertion cases.** The dedicated
  workload asserts masked-at-deadline placement, FIFO equal deadlines, a repeated
  identity after unmask, and an already-due timer whose first eligible exit is WFI.
- **PASS — identical scripts produce identical complete normalized logs.** Both
  logs also pass the independent schedule checker.
- **PASS — perturbed scripts diverge at the exact index.** Separate committed
  tests perturb a V-ns increment, interrupt placement, and a byte of checkpoint
  state.

### Does-not-count-unless criteria

- **PASS — comparator failure proof: V-ns.** One increment is changed by one V-ns;
  the comparator reports event 1 / `VnsAfter`.
- **PASS — comparator failure proof: interrupt placement.** One interrupt moves one
  exit late; the comparator reports event 0 / `Interrupts`.
- **PASS — comparator failure proof: full state.** One byte of retained guest
  register state is flipped before checkpoint hashing; the comparator reports
  event 1 / `StateHash`.
- **PASS — placement checker catches consistently late twins.** Every deadline in
  a two-deadline script is moved one exit late. The two perturbed logs compare
  equal to each other, while the placement checker rejects event 0.
- **PASS — duplicate and missing delivery failures.** Both are committed negative
  tests and fail at the exact event.

### M0 command evidence

`cargo test -p vmm-core --test prescriptive_vtime --all-features -- --nocapture`

```text
running 10 tests
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

`cargo test -p vmm-core --all-features`

```text
vmm-core unit tests: test result: ok. 491 passed; 0 failed; 2 ignored
arm64_skeleton: test result: ok. 14 passed; 0 failed
event_loop: test result: ok. 19 passed; 0 failed
prescriptive_vtime: test result: ok. 10 passed; 0 failed
snapshot_branch: test result: ok. 8 passed; 0 failed
all other non-hardware test binaries: 0 failures
```

`cargo clippy -p vmm-core --all-features --all-targets -- -D warnings`

```text
Finished `dev` profile [unoptimized + debuginfo]
exit status 0
```

`cargo fmt --all -- --check`

```text
exit status 0
```

`cargo +nightly-2026-06-16 public-api -p vmm-core --all-features --target
x86_64-unknown-linux-gnu -sss --color never` compared with the Linux-frozen
snapshot on macOS:

```text
byte-for-byte match; diff exit status 0
```

`cargo llvm-cov nextest --all-features ... --fail-under-regions 90`

```text
1234 tests run: 1234 passed, 25 skipped
workspace regions: 94.76%
vmm-core/src/prescriptive.rs regions: 90.08%
report/floor exit status 0
```

`MIRIFLAGS=-Zmiri-permissive-provenance cargo +nightly-2026-06-16 miri test -p vmm-core`

```text
RUNNING — required before M0 is declared fully green.
```

**M0 overall: FAIL (verification still running).** Native implementation and all
positive/negative oracles are green; the required Miri gate has not completed yet.
M1 has not started.

## M1 — the M1 Max boots deterministically

- **FAIL — probe binary and recorded HVF findings:** not started.
- **FAIL — `HvfBackend`, userspace GICv3 delivery, WFI/IdlePlanner:** not started.
- **FAIL — paravirtual tick patch and `/init` boot:** not started.
- **FAIL — ten same-seed full-boot normalized logs:** not started.
- **FAIL — placement checker green for every boot:** not started.
- **FAIL — no liveness-watchdog abort:** not started.
- **FAIL — one-exit-late tick comparator and consistent-error placement negatives:**
  not started.
- **FAIL — every retained state class perturbs the hash and round-trips restore:**
  not started.
- **FAIL — exclusive-monitor canonicalization backed by the LL/SC image audit:**
  not started.
- **FAIL — honest `capabilities()` surface:** not started.

## M2 — NES campaign on the M1 Max

- **FAIL — guest payload and control-protocol `Machine` client:** not started.
- **FAIL — two same-seed archive hashes:** not started.
- **FAIL — every archived lineage replays byte-for-byte:** not started.
- **FAIL — snapshot restore counter and uninterrupted-continuation hash oracle:**
  not started.
- **FAIL — in-process/guest/transport cross-build differential:** not started.
- **FAIL — thousands of mid-workload branch/replay cycles:** not started.
- **FAIL — altered-chord archive comparator negative:** not started.
- **FAIL — RAM, vCPU, and GIC/device stored-snapshot corruption negatives:** not
  started.

## M3 — liveness on a real payload

- **FAIL — postgres acceptance payload under prescriptive V-time:** not started.
- **FAIL — acceptance checks, watchdog, dmesg health:** not started.
- **FAIL — inter-exit V-ns histogram and bounded maximum:** not started.
- **FAIL — throughput beside descriptive x86:** not started.
- **FAIL — report demonstrates an unbounded gap and severe slowdown:** not started.

## M4 — instrumented concurrency payload

- **FAIL — SDK threshold protocol:** not started.
- **FAIL — deliberately racy Go/Rust suite with known schedules:** not started.
- **FAIL — deterministic seeded reproduction:** not started.
- **FAIL — held-out schedule discovery within predeclared budgets:** not started.
- **FAIL — wrong-schedule negative for every entry:** not started.
- **FAIL — held-out schedules absent from seeds/fixtures and per-bug report:** not
  started.

## Follow-up F1/F2 (out of current scope)

- **BLOCKED — F1 arm64 KVM delivery:** waits for the `msr1` box to be idle, per the
  plan of record.
- **BLOCKED — F2 cross-host portability:** depends on F1 and the same idle box.

## Repository-wide final gates

- **PASS at M0 — `cargo build --all-features`:** exit status 0.
- **PASS at M0 — `cargo nextest run --all-features`:** 1234 passed, 25 skipped.
  The sandboxed first attempt could not open telemetry's local test socket; the
  unrestricted rerun passed every test.
- **PASS at M0 — `cargo clippy --all-features --all-targets -- -D warnings`:**
  exit status 0 (the pre-existing invalid-path notices from `clippy.toml` remain
  non-fatal).
- **PASS — `cargo fmt --all -- --check`:** green at M0.
- **PASS at M0 — `cargo deny check`:** advisories, bans, licenses, and sources all
  green.
- **PASS at M0 — Linux-frozen `vmm-core` public API:** exact cross-target match.
- **PASS at M0 — coverage ratchet:** 94.76% workspace region coverage against the
  workflow's 90% floor; the new module measures 90.08%.
- **FAIL — mutation, Kani, cross-arch, and remaining quality-toolchain gates:**
  not yet run for the complete plan.
