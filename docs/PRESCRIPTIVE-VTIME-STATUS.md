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
5. **Mutation findings become explicit anti-vacuity oracles.** The first shard-0
   mutation run found that tests did not observe event-class domain separation or
   the raw-log accessor. The suite now proves all seven event classes produce
   distinct digests for identical payload bytes and checks the complete raw log.
   Shard 2 then exposed the missing post-first-event V-time-regression oracle; an
   exact event-1 failure test was added. The redundant `position > 0` guard was
   removed because its `>= 0` mutant is equivalent for a `usize`, while comparing
   every event against a zero initial baseline is total and identical in meaning.
6. **Prescriptive proptests follow the crate's Miri convention.** Native runs keep
   256 cases. Under Miri they use 16 interpreted cases and disable cwd-backed
   failure persistence, which Miri isolation does not support. The first full
   Miri run established all earlier unit/integration binaries were clean and
   exposed only this harness incompatibility; the repaired prescriptive target
   and every remaining integration target then ran clean under Miri.
7. **The HVF probe invalidates rewritten instruction-cache lines.** The first
   multi-program probe reused one IPA without a host instruction-cache flush,
   allowing stale guest instructions to make later observations vacuous. Every
   rewrite now calls `sys_icache_invalidate`; all recorded trap syndromes below
   come from the repaired, entitlement-signed run.
8. **HVF's measured GIC CPU-interface trap is the delivery mechanism.** The
   repaired probe records stable traps for `ICC_IAR1_EL1`, `ICC_EOIR1_EL1`,
   `ICC_PMR_EL1`, and `ICC_IGRPEN1_EL1`; `ICC_SRE_EL1` executes directly. The
   backend canonicalizes the trap ISS by clearing only Rt and direction, and
   vmm-core dispatches the ruled identities to the userspace GIC. Unknown
   identities still fail before any fabric-wiring check.

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
- **PASS — post-first-event V-time regression failure.** A `5 -> 4` regression at
  event 1 returns the exact `VtimeRegressed` evidence.

### M0 command evidence

`cargo test -p vmm-core --test prescriptive_vtime --all-features -- --nocapture`

```text
running 12 tests
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
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

`cargo mutants --no-shuffle --in-diff /private/tmp/prescriptive-m0.diff --shard N/4`

```text
shard 3: 12 mutants tested: 11 caught, 1 compiler-rejected
shard 0 first pass: 3 missed (the two oracle gaps recorded above)
shard 0 repaired rerun: 14 mutants tested: 7 caught, 7 compiler-rejected
shard 1: 14 mutants tested: 9 caught, 5 compiler-rejected
shard 2 first pass: 3 missed (one regression-oracle gap)
shard 2 after the oracle: 1 equivalent mutant remained; redundant guard removed
shard 2 final rerun: 13 mutants tested: 13 caught
final total: 53 mutants, 40 caught, 13 compiler-rejected, 0 missed, 0 timed out
```

`MIRIFLAGS=-Zmiri-permissive-provenance cargo +nightly-2026-06-16 miri test -p vmm-core`

```text
library: 394 passed, 99 intentionally ignored, 0 failed
arm64_skeleton: 14 passed; corpus_oracle_mock: 3 passed; event_loop: 19 passed
linux_loader_proptest: 4 passed; loader_proptest: 3 passed
first prescriptive attempt: harness-only getcwd isolation error, no UB finding
repaired `--test prescriptive_vtime`: 12 passed, 0 failed
remaining protocol target: 5 passed, 0 failed
public_api/seal_rate_sweep/snapshot_branch: clean (ignored/zero tests under Miri)
composite result: every Miri-enabled vmm-core unit and integration target clean
```

**M0 overall: PASS.** Every build, positive oracle, negative oracle, independent
placement check, mutation shard, Miri-enabled target, portable/full native gate,
Linux-frozen API check, and aarch64 seam check is green. M1 may now start; no M1
work was begun before this evidence was recorded.

## M1 — the M1 Max boots deterministically

- **PASS — probe binary and recorded HVF findings:**
  `consonance/vmm-backend/src/bin/hvf_probe.rs` compiled and ran in one
  entitlement-signed process on the M1 Max / macOS 26.4.1 host. The exact
  surface and its backend consequences are recorded in
  `consonance/vmm-backend/IMPLEMENTATION.md`.

  ```text
  state.scalar: 35/35 get+set; X0 and FPCR/FPSR perturbations exact
  state.simd-fp: Q0 perturbation exact
  state.sysregs: 18/18 get+set; TPIDR_EL0 and CNTV_CVAL perturbations exact
  state.debug: 65/65 get+set; DBGBVR0 and both trap-control toggles exact
  state.pending-irq: true and false read back exactly
  state.pending-exception / exclusive-monitor: no public get/set API
  trap.cntvct-el0: false; returned a live nonzero host-derived counter
  trap.pmccntr-el0: true, EC=0x18
  trap.midr-el1 / cntv-cval-el0: false
  exit.unmapped-mmio: EC=0x24, VA=IPA=0x20000
  interrupt.pre-entry: PC=0x284, ELR_EL1=0x0
  exit.wfi: EC=0x1, syndrome=0x07e00000, PC=0x0
  trap.icc-iar1-el1: syndrome=0x62303019
  trap.icc-eoir1-el1: syndrome=0x62323038
  trap.icc-pmr-el1-write/read: syndrome=0x6230104c / 0x6230106d
  trap.icc-igrpen1-el1: syndrome=0x623e3098
  trap.icc-sre-el1: false
  ```

  The negative findings are capability limits, not papered-over passes: M1's
  cooperative image must use paravirtual time, and the eventual HVF capability
  report must deny direct-counter and timer-sysreg enforcement. WFI and MMIO
  have measured exception exits suitable for the backend.
- **IN PROGRESS — `HvfBackend`, userspace GICv3 delivery, WFI/IdlePlanner:** the
  production backend now creates/maps/runs an HVF vCPU, surfaces WFI as `Idle`,
  decodes MMIO and the measured GIC sysreg traps, supports pre-entry IRQ levels,
  handles the uniprocessor PSCI subset, and is composed with the userspace GIC.
  The HVF root deliberately omits the legacy 8-KiB doorbell mapping because HVF
  requires 16-KiB mappings and M1 has no control channel. Integration with the
  prescriptive `IdlePlanner` remains outstanding.
- **IN PROGRESS — paravirtual tick patch and `/init` boot:** the pinned
  Linux 6.18.35 arm64 Image and initramfs now build natively in the audited
  container. The maintained counter/timer and exclusive-instruction scanners
  reject planted live instructions before accepting the real Image, vDSO, and
  initramfs. The composition root places the initramfs in checked guest RAM and
  publishes its exact range in `/chosen`. An entitlement-signed, event-bounded
  `hvf_boot` run reaches Linux 6.18.35 on `harmony-arm64-virt`; it currently
  stops at the first ARM pvclock registration write after Linux reports that no
  GIC distributor was detected. These are precise modeled-surface failures,
  not a claimed boot pass. The new prescriptive paravirtual tick patch and
  `/init` ready marker remain outstanding.
- **FAIL — ten same-seed full-boot normalized logs:** not started.
- **FAIL — placement checker green for every boot:** not started.
- **FAIL — no liveness-watchdog abort:** not started.
- **FAIL — one-exit-late tick comparator and consistent-error placement negatives:**
  not started.
- **IN PROGRESS — every retained state class perturbs the hash and round-trips
  restore:** SIMD/FP, debug registers and trap controls, virtual-timer
  register/mask/offset state, and pending IRQ/FIQ now ride backend state,
  `vm-state` TLVs, vmm-core conversion, the canonical VCPU hash, and labeled
  diagnostic hashes. Codec round-trip uses distinctive values; strict decode
  rejects noncanonical booleans and reserved bytes; one-field hash perturbations
  localize to exactly the corresponding class. A live HVF save/restore oracle
  remains outstanding.
- **FAIL — exclusive-monitor canonicalization backed by the LL/SC image audit:**
  the image-side audit is now non-vacuous and green: planted `LDXR`/`STXR`
  instructions are rejected with the expected rows, while the real vmlinux,
  vDSO, and initramfs contain no LL/SC instructions. Backend canonicalization
  and its live restore oracle remain outstanding.
- **PASS — honest `capabilities()` surface:** `HvfBackend` reports both
  `deterministic_cntvct` and `enforces_cntv_cval` false, matching the probe's
  direct `CNTVCT_EL0` and `CNTV_CVAL_EL0` results; it never calls `run_until`.

### M1 checkpoint command evidence

`cargo nextest run -p gicv3 -p vm-state -p vmm-backend -p vmm-core
--all-features`

```text
709 tests run: 709 passed, 8 skipped
```

`cargo clippy -p gicv3 -p vm-state -p vmm-backend -p vmm-core --all-features
--all-targets -- -D warnings`

```text
exit status 0 (pre-existing clippy.toml invalid-path notices only)
```

The intentional additive `vm-state` and Linux-frozen `vmm-backend` API changes
were regenerated with the pinned `nightly-2026-06-16` public-api tool and the
diff contains only the new arm64 state classes and fields.

The first native guest-build checkpoint produced these ignored artifacts:

```text
Image:                    857bc1c59666f8e2dc3a17f0b40f6db0b3c4e899f0cae6f12238d02fb32e0cac
initramfs.cpio.gz:        d1ccc8d7cea812095bdf5cc77c4ac505c6a22f16b666f1ef111eb1317be67968
initramfs-el0probe.cpio.gz: 292a1f40b428545b65706e5512e89f6754d0b62e8174de9b550c308ff0a8ba5a
```

`cargo nextest run -p vmm-core --all-features` passed 563/563 tests (4
skipped), and the matching all-targets Clippy invocation completed with no
diagnostics beyond the repository's pre-existing invalid-path notices.

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
- **PASS at M0 — `cargo nextest run --all-features`:** 1236 passed, 25 skipped.
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
- **PASS at M0 — mutation gate:** all 53 changed-code mutants accounted for; no
  survivors or timeouts.
- **PASS at M0 — aarch64 architecture seam:** full all-feature/all-target clippy
  for `aarch64-unknown-linux-gnu`, exit status 0.
- **FAIL — Kani and remaining final quality-toolchain gates:** not yet run for the
  complete plan.
