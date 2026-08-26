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
9. **The arm64 pvclock page is reserved but remains in the linear map.** The
   host stamps ordinary shared RAM while the sole vCPU is stopped. Marking the
   DT reservation `no-map` forced Linux to create a device-memory alias for the
   same physical page and the guest never observed the valid host frame. The DT
   now reserves the page from the allocator without `no-map`, and a committed
   structural test rejects reintroducing that incoherent alias.
10. **Early pvclock validation uses the DT contract, not an uninitialized timer
    global.** `harmony_arm_pvclock_register()` runs before
    `arch_timer_of_configure_rate()`. Comparing the host frame against
    `arch_timer_get_rate()` therefore compared `62,500,000` against zero and
    produced the same panic as an absent stamp. The guest now reads and validates
    the timer node's nonzero `clock-frequency` property directly before accepting
    the page. The live run prints the accepted page and proceeds to the first
    clockevent control write.
11. **The ARM clockevent is a snapshotted level input, not an edge helper.** Its
    absolute guest-clock deadline, PPI20 line level, assertion/ACK counters, and
    prescriptive-mode bit ride the ARM device blob and hash. EOI without device
    ACK re-pends the interrupt; ACK/DISARM lower pending state but leave an
    already-active interrupt for architectural EOI. Malformed combinations and
    descriptive/prescriptive restore mismatches fail before mutation.
12. **HVF traps Linux's two OS debug-lock zero writes.** Live fail-closed boots
    identified canonical sysregs `0x00280406` and `0x00280400`; reconstructing
    and disassembling their architectural encodings identified `OSDLR_EL1` and
    `OSLAR_EL1`. The contract accepts only Linux's deterministic zero unlock,
    assigns the explicit architectural-control exit duration, and rejects reads
    or nonzero writes. Retained debug registers remain the only stateful debug
    surface.
13. **The minimal init reports through `/dev/kmsg`.** This board retains PL011
    as the early boot console and intentionally has no registered ttyAMA console,
    so PID 1 inherits closed standard descriptors. The owned freestanding init
    falls back to a reproducibly packed `/dev/kmsg` node; printk forwards its
    deterministic readiness markers to the captured early console. The boot
    harness matches the complete marker independent of kernel `LF -> CRLF`
    transport translation.
14. **A milestone log is a finite prefix, so future deadlines remain honest.**
    The first production placement run ended at `/init` with V-time
    `14,141,000` and one still-armed kernel tick at `17,528,000`. Requiring that
    not-yet-eligible deadline to have fired is impossible without extending the
    run past its observation marker. The checker now permits only an uncanceled
    deadline strictly beyond the final V-time; a deadline due anywhere in the
    prefix is still rejected at its first eligible event. A committed positive
    and negative unit test fixes both halves, and the plan text now states this
    finite-prefix rule explicitly.
15. **The ARM serial contract assigns 2,000 V-ns per access.** The original
    1,000 V-ns row reached `/init` before the kernel's first tick, leaving no
    production interrupt placement to compare. A diagnostic guest-side padding
    experiment was rejected because synchronous printk itself became
    exit-starved and correctly tripped the watchdog. Assigning the normal PL011
    class 2,000 V-ns makes the unchanged audited boot traffic cross the first
    deadline; this is a normative, host-independent per-class constant, not a
    workload-specific exit or a host-time measurement.
16. **Prescriptive WFI uses the paravirtual clockevent through `IdlePlanner`.**
    Assigned-at-exit mode does not require or claim a deterministic hardware
    counter. The ARM deadline seam converts the absolute guest-clock deadline
    to its first whole V-ns, filters PPI20 through the same GIC Group-1/enable/
    priority/active gates as real delivery, and lands exactly through
    `IdlePlanner`; post-exit service raises PPI20 at that normalized event.
    Descriptive preemption remains separately gated, so prescriptive mode never
    calls the backend's honestly unsupported `run_until`.
17. **The exclusive monitor is an image-admission invariant, not fabricated
    backend state.** HVF exposes no monitor save, restore, or clear operation.
    A new vCPU begins with an empty monitor, and the mandatory whole-image
    `aa4-exclusive-scan.py` gate proves that the admitted kernel, vDSO, and init
    cannot execute LL/SC to make it nonempty. The backend therefore documents
    canonical-empty at every sealable boundary and deliberately omits an
    unenforceable bit from `Arm64VcpuState`. Pending synchronous injection is
    handled analogously but more strictly: `save` and `restore` reject any
    staged backend completion, while the retained interrupt class carries the
    externally asserted IRQ/FIQ levels exactly.
18. **The arm64 hypercall control slot is one fully retained 16-KiB mapping.**
    HVF requires the guest-memory slot to follow the host's 16-KiB page shape;
    the earlier 8-KiB request/response-only proposal is not mappable on the
    measured M1 Max. The canonical allocation starts at GPA `0xC000`, preserving
    the frozen request and response GPAs `0xE000`/`0xF000`. All four pages,
    including the alignment padding, enter snapshots and `state_hash`. The
    entitlement-signed production-composition probe accepted the mapping and
    reported `HVF_CONTROL_MAP_OK bytes=16384` with state hash
    `0766544d87f6924a70fc1bb9755be1846f12f08b02a353d28677e66a7701eea4`.
19. **M2 payload input is an optional sequential tape whose state is its remaining
    suffix.** Input chords do not have predictable `Moment`s, so they do not belong
    in the sparse override map. Environment blob version 7 adds an ordered tape:
    absent means service unavailable, empty means offered and exhausted, and an
    exact-length request consumes one entry. Snapshots, the SDK state hash, and the
    `RecordedEnv` reply all carry only the unconsumed suffix. Mutation preserves the
    tape; Moment-based `EnvCodec::compose` rejects it because prefix consumption
    cannot be inferred safely. This keeps the VMM workload-blind and makes guest,
    snapshot, replay, and in-process implementations share one reproducer byte form.
20. **PR #193 lands additively and both emulator builds consume environment v7.**
    The current `machine` and `searcher` crates from
    `exec/pr193-boundary-revisions` are workspace members beside the existing
    LibAFL `fuzzer`; the import does not replace that work. Phase 5 permits the
    `machine` crate's first harmony dependencies. Its in-process `NesMachine` now
    parses the same canonical version-7 ordered-payload reproducer the consonance
    socket path sends, rather than a private pair blob. The synchronous
    `SocketMachine` validates exact capabilities and request sequence, preserves
    control errors, retains seal cuts, and reports genesis versus continuation
    restore counts.
21. **M2 game deadlines are an SDK frame clock, never wire V-time.** Lifecycle
    local id 1 is `frame_complete([frame_count u64-le])`. The VMM accepts only
    the exact eight-byte payload and defers a `SnapshotPoint`; the socket client
    independently validates a strictly increasing counter, sends no control-wire
    deadline, and advances until the first chord-boundary report at or beyond the
    requested frame. The following payload fetch is the synchronized pre-consume
    boundary, so a continuation snapshot never resumes from an already-failed
    exhaustion response.
22. **M2 has a distinct userspace-capable image, with no relaxation of M1's
    executable-image admission rule.** The sealed minimal M1 `Image` and object
    tree remain untouched. `Image-game` layers only the proc/futex/devmem/tmpfs
    facilities needed by std/TetaNES, retains every clock/interrupt/LSE exclusion,
    and passes through the same planted-negative counter and exclusive scanners.
    The initramfs builder scans the complete TetaNES binary, BusyBox, dynamic
    loader, and every shared object. A generic runtime containing dormant LL/SC
    therefore blocks the build loudly rather than being waived on an LSE host.
23. **The SMB campaign has one policy path and a bounded live-handle cache.**
    `SmbGame` is parameterized only by target construction; the archive,
    selector, mutation draw, admission probe, milestone merge, and parent policy
    remain the same generic implementation for in-process and control-socket
    execution. A remote snapshot serializes its complete chord lineage and WRAM
    evidence but skips its session-local server handle. Each live session retains
    at most 1,024 continuation handles; eviction drops the VMM snapshot, and a
    later restore deterministically reconstructs from gameplay genesis. This
    prevents rejected or old archive candidates from leaking unbounded server
    snapshots without making raw handles falsely portable across sessions.
24. **Restore accounting is deterministic stream evidence, not a live-only
    sidecar.** Every nonzero job record carries separate genesis and continuation
    restore deltas; replay recomputes and compares them before admission, and the
    report accumulates bootstrap plus job counts. Zero counts are omitted and
    default on decode, preserving byte-for-byte local streams and historical
    recordings whose targets did not expose counters. The throughput sidecar
    additionally reports continuation restores per wall second, but wall time
    never enters the stream or deterministic report.
25. **Snapshot content addresses are verified on restore, including opaque
    machine state.** A page's resident bytes are re-hashed against its sealed
    BLAKE3 content address before read or materialization. Each vCPU/device blob
    carries its own seal-time BLAKE3 digest and is verified before the vendor
    codec sees it. Missing or changed data fails as a typed integrity error; it
    never degrades to a zero page or partially restores a fresh VM. Test-only
    corruption hooks are feature-gated and cannot enter a production build.

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
- **PASS — `HvfBackend`, userspace GICv3 delivery, WFI/IdlePlanner:** the
  production backend now creates/maps/runs an HVF vCPU, surfaces WFI as `Idle`,
  decodes MMIO and the measured GIC sysreg traps, supports pre-entry IRQ levels,
  handles the uniprocessor PSCI subset, and is composed with the userspace GIC.
  The HVF root deliberately omits the legacy 8-KiB doorbell mapping because HVF
  requires 16-KiB mappings and M1 has no control channel. Integration with the
  prescriptive idle path now folds the generic timer and paravirtual clockevent,
  checks the future input's actual GIC deliverability, and lands exactly at the
  earliest deadline through `IdlePlanner`. The committed mock-ARM test reaches
  the deadline, logs PPI20 on the WFI event, passes the placement checker, and
  succeeds only because no unsupported `run_until` call occurs.
- **PASS — paravirtual tick patch and one complete `/init` boot:** the pinned
  Linux 6.18.35 arm64 Image and initramfs now build natively in the audited
  container. The maintained counter/timer and exclusive-instruction scanners
  reject planted live instructions before accepting the real Image, vDSO, and
  initramfs. The composition root places the initramfs in checked guest RAM and
  publishes its exact range in `/chosen`. An entitlement-signed, event-bounded
  `hvf_boot` run reaches Linux 6.18.35 on `harmony-arm64-virt`; it currently
  identifies the userspace GICv3 distributor, its single redistributor, and 64
  SPIs. Exact PIDR2, 64-bit GICR_TYPER, and single-affinity GICD_IROUTER
  behavior were added from those fail-closed exits. The host now discovers the
  page's checked DT placement (`0x40311000`), stamps it at prescriptive exits,
  and Linux reports `Harmony pvclock: registered page 0x40311000 (ABI 1)`.
  The exact deadline/DISARM/ACK protocol now drives level-triggered PPI20 through
  the userspace GIC; portable tests cover EOI-without-ACK reassertion, fail-closed
  protocol misuse, state hashing, snapshot/restore, and mode mismatch. Live
  fail-closed iteration then identified Linux's OSDLR/OSLAR zero writes. With
  those exact dispositions and deterministic `/dev/kmsg` init output, the signed
  HVF run reaches `HARMONY_AA5_CLOCKSOURCE_OK` and `HARMONY_AA5_READY` at event
  14,140 before the watchdog.

  ```text
  Image sha256:     41cea2eb60e4155b31ac70300ff9c15205b1533a7b7ab9fb7642bdb17628a3c7
  initramfs sha256: 6194ec4be99b08e68a61f9020fcedd7aae515b00fa63d38a44b9070a23fea053
  HVF_BOOT_READY event=14140
  state_hash=6949042e3fd067b9610c2ed46fffcb720ae04462bfafb340533ea54cb43a1e60
  exit status 0; liveness watchdog did not fire
  ```
- **PASS — ten same-seed full-boot normalized logs:** all ten signed optimized
  HVF boots recorded the same 14,141 exits, assigned V-time at every event,
  payload digests, one PPI20 placement, 55 interval checkpoints plus the
  `/init` checkpoint, final state hash, and canonical log digest. The harness
  compared the complete normalized text logs byte-for-byte in addition to
  requiring the compact summaries to match.
- **PASS — placement checker green for every boot:** all ten production logs
  passed independently against their deadline schedules with one real PPI20
  delivery each.
- **PASS — no liveness-watchdog abort:** all ten boots reached `/init`; the
  harness scanned stdout and stderr and reports `watchdogs=0`.
- **PASS — one-exit-late tick comparator and consistent-error placement
  negatives on the production workload:** `hvf_boot` moves every live delivery
  one exit late, proves two identically late twins compare equal, then requires
  the original-vs-late comparator and independent placement checker to reject
  the exact same first event (`12,529`).
- **PASS — every retained state class perturbs the hash and round-trips
  restore:** the portable class matrix independently changes core registers,
  sysregs, SIMD/FP, debug registers and both trap controls, virtual-timer
  register/mask/offset state, pending IRQ/FIQ, PL011 device state, userspace-GIC
  state, assigned V-time, and seeded-entropy state. Each change alters only its
  named diagnostic component, changes the canonical hash, and reproduces the
  exact snapshot and hash through restore. The pre-existing continuation oracle
  additionally proves that entropy resumes at the captured stream position
  rather than replaying its first word. The entitlement-signed live
  `hvf_state_oracle` then perturbed and exactly restored all six HVF-retained
  backend classes, returning to the exact baseline after every class.
- **PASS — exclusive-monitor canonicalization backed by the LL/SC image audit:**
  the image-side audit is non-vacuous and green: planted `LDXR`/`STXR`
  instructions are rejected with the expected rows, while the real vmlinux,
  vDSO, and initramfs contain no LL/SC instructions. The backend and retained-
  state table now state the enforceable canonical-empty invariant at every
  sealable boundary; no synthetic, unrestorable monitor field is claimed.
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

The DT-discovered page checkpoint rebuilt Linux 6.18.35 from the verified
pristine tarball in the native arm64 container. Both planted negatives fired,
and all real-image audits passed:

```text
counter scanner: planted live-counter probe rejected
vmlinux/vDSO: no live counter reads; 3 allowed CNTFRQ reads; no CVAL/TVAL
exclusive scanner: planted LDXR/STXR probe rejected
vmlinux/vDSO: no LL/SC instructions
Image sha256: 41cea2eb60e4155b31ac70300ff9c15205b1533a7b7ab9fb7642bdb17628a3c7
initramfs.cpio.gz sha256: d1ccc8d7cea812095bdf5cc77c4ac505c6a22f16b666f1ef111eb1317be67968
```

`cargo nextest run -p vmm-core -p vmm-backend --all-features` passed
656/656 tests (5 skipped), followed by an all-targets `clippy -D warnings`
success (only the repository's pre-existing invalid-path notices) and a clean
`cargo fmt --all -- --check`.

First production normalized-log oracle after wiring the run loop and selecting
the 2,000 V-ns PL011 row:

```text
HVF_M1_ORACLE events=14141 raw=14141 schedules=1 deliveries=1 checkpoints=56
placement=ok late_comparator_event=12529 late_placement_event=12529
log_digest=8604f41cd2373d0bb582982119d168c6da310098bd35f2005fadbd161e033a3e
HVF_BOOT_READY event=14140
state_hash=d9576a79a323fda8ee5aa49b9c998d7ace7d17d7f588f7d76f01368affa16af3
exit status 0; liveness watchdog did not fire
```

Final-head ten-run corpus (`/private/tmp/harmony-m1-ten-bbc7d3d3`), rerun after
the WFI and retained-state commits:

```text
runs 01..10, each:
  events=14141 raw=14141 schedules=1 deliveries=1 checkpoints=56
  placement=ok late_comparator_event=12529 late_placement_event=12529
  log_digest=8604f41cd2373d0bb582982119d168c6da310098bd35f2005fadbd161e033a3e
  HVF_BOOT_READY event=14140
  state_hash=d9576a79a323fda8ee5aa49b9c998d7ace7d17d7f588f7d76f01368affa16af3
Image sha256:     41cea2eb60e4155b31ac70300ff9c15205b1533a7b7ab9fb7642bdb17628a3c7
initramfs sha256: 6194ec4be99b08e68a61f9020fcedd7aae515b00fa63d38a44b9070a23fea053
M1_TEN_RUN_ORACLE_OK normalized_logs=10 watchdogs=0
```

WFI/IdlePlanner integration checkpoint:

```text
cargo nextest run -p gicv3 -p vmm-core --all-features
592 tests run: 592 passed, 5 skipped
cargo clippy -p gicv3 -p vmm-core --all-features --all-targets -- -D warnings
exit status 0 (pre-existing clippy.toml invalid-path notices only)
gicv3 Linux-frozen public API: byte-for-byte match
```

Retained-state completeness checkpoint:

```text
cargo nextest run -p vmm-backend -p vmm-core --all-features
665 tests run: 665 passed, 5 skipped

cargo clippy -p vmm-backend -p vmm-core --all-features --all-targets -- -D warnings
exit status 0 (pre-existing clippy.toml invalid-path notices only)

cargo fmt --all -- --check
exit status 0

entitlement-signed target/release/hvf_state_oracle:
HVF_STATE_CLASS_OK class=general
HVF_STATE_CLASS_OK class=simd-fp
HVF_STATE_CLASS_OK class=sysregs
HVF_STATE_CLASS_OK class=debug
HVF_STATE_CLASS_OK class=vtimer
HVF_STATE_CLASS_OK class=pending-interrupts
HVF_STATE_ROUNDTRIP_OK classes=6 baseline_restores=6
```

**M1 overall: PASS.** The measured Apple-HVF backend, audited cooperative image,
paravirtual clocksource and clockevent, userspace GICv3, exact WFI landing,
complete retained-state/hash/restore matrix, canonical-empty exclusive monitor,
ten same-seed full normalized logs, independent delivery placement check, live
comparator and placement negatives, and liveness watchdog criterion are all
green on the current M1 head. M2 did not begin before this evidence was recorded.

## M2 — NES campaign on the M1 Max

- **IN PROGRESS — guest payload and control-protocol `Machine` client:** the
  architecture-native arm64 `MmioDoorbell` and the HVF-aligned, fully retained
  control-memory composition are implemented. The focused portable suite passed
  583/583 tests (5 skipped), and the signed live probe produced the measured
  `HVF_CONTROL_MAP_OK` evidence recorded in decision 18. The generic environment,
  doorbell protocol, VMM service, snapshot/replay carry, live-suffix reproducer,
  and control-loop negative oracles are implemented. The focused substrate suite
  is green (environment, hypercall-proto, and vmm-core), including the control test
  that alters one chord and proves the whole-state comparator fires. The arm64
  generic control-protocol `Machine` client is implemented and closes against a
  real `ControlServer<MockBackend>` over a Unix socket. The imported PR #193
  searcher remains green against the shared version-7 bytes. The portable arm64
  guest agent now runs the same pinned TetaNES configuration, consumes exactly
  one payload per chord, mirrors complete WRAM, and reports cumulative frames at
  chord boundaries. Its live Linux/aarch64 `/dev/mem` composition and separate
  fail-closed image builder are present. The ROM, native Altra image build, and
  live M1-Max execution remain open, so this criterion is not yet a pass. The
  searcher now exposes the same campaign through `--control-socket`, requires
  exactly one worker per mutable VMM session, records a distinct durable remote
  checkpoint format, and can replay a recorded stream through a fresh session.

### M2 payload-substrate checkpoint evidence

```text
cargo nextest run -p environment -p hypercall-proto -p vmm-core --all-features
715 tests run: 715 passed, 6 skipped

cargo clippy -p environment -p hypercall-proto -p vmm-core \
  --all-features --all-targets -- -D warnings
exit status 0 (pre-existing clippy.toml invalid-path notices only)

cargo fmt --all -- --check
exit status 0

pinned public API: environment and hypercall-proto regenerated then matched;
vmm-core macOS output confirms the sole platform-neutral addition
SdkStop::Quiescent (the committed Linux-frozen snapshot contains that line)
```

The positive control oracle consumed `[0x81, 4]`, observed only `[0, 2]` in
`RecordedEnv`, sealed that suffix, consumed it, replayed the seal, and consumed
the identical suffix again. Its two anti-vacuity arms changed `[0x81, 4]` to
`[0x81, 5]` and required the whole-state hash to differ, then drove an exhausted
tape through a real mock PIO exit and required `StopReason::Quiescent` with
`StopMask::NONE`.

```text
cd dissonance
cargo test -p machine
10 passed (9 unit + 1 real control-server socket integration)

cargo test -p searcher --lib
50 passed; 0 failed (699.64 s; includes archive/replay and seed-sweep tests)

cargo clippy -p machine -p searcher --all-targets -- -D warnings
exit status 0 (pre-existing clippy.toml invalid-path notice only)

cargo fmt --all -- --check
exit status 0
```

### M2 frame-clock and guest-agent checkpoint evidence

```text
cargo test --manifest-path harmony-linux/sdk/Cargo.toml
11 passed; 0 failed; 1 ignored

cargo nextest run -p vmm-core --all-features \
  -E 'test(classify_sdk_event_payload_matrix) | \
      test(doorbell_rejects_malformed_sdk_event_payloads)'
2 passed; 0 failed

cd dissonance && cargo test -p machine
12 passed (11 unit + 1 real control-server socket integration)

cargo test --manifest-path harmony-linux/tetanes-agent/Cargo.toml --locked
3 passed; 0 failed

cargo check --manifest-path harmony-linux/tetanes-agent/Cargo.toml \
  --target aarch64-unknown-linux-gnu --locked
exit status 0

cargo clippy --manifest-path harmony-linux/tetanes-agent/Cargo.toml \
  --all-targets -- -D warnings
exit status 0 (pre-existing clippy.toml invalid-path notice only)
```

The committed negatives reject seven- and nine-byte `frame_complete` payloads
at the VMM, malformed or decreasing frame reports at the socket client, a frame-4
yield for a frame-5 deadline, malformed WRAM registration, absent/hidden/
overflowing pagemap entries, and zero or overlong chord holds. The portable
guest differential compares an independently configured TetaNES deck's full
2-KiB WRAM at every chord boundary.

### M2 searcher-adapter checkpoint evidence

```text
cd dissonance
cargo test -p searcher --lib smb::remote
4 passed; 0 failed

cargo test -p searcher --lib \
  smb::campaign::tests::live_campaign_replays_byte_identically
1 passed; 0 failed

cargo clippy -p searcher --lib -- -D warnings
exit status 0 (pre-existing clippy.toml invalid-path notice only)

cargo check -p searcher --bin smb-campaign
exit status 0

cargo test -p searcher --lib \
  restore_accounting_is_recorded_replayed_and_tamper_evident
1 passed; 0 failed
```

The adapter differential executes the same synthetic NROM and chords through
independent in-process and control-machine boundaries and compares complete WRAM
and frame count at every chord endpoint. Its negatives reject a 2,047-byte WRAM
publication and a one-frame guest-report offset. Snapshot tests prove both a
live continuation restore and a post-serialization genesis-lineage restore; a
1,025-snapshot churn test evicts the oldest live handle and proves the same
durable fallback remains byte-exact. The restore-accounting oracle runs a live
and serially replayed campaign through a counting target, requires nonzero and
class-separated totals, then increments one recorded continuation count and
requires replay to reject that exact job.

The stored-snapshot integrity matrix is also green:

```text
cargo test -p snapshot-store --features test-utils \
  sealed_page_and_vm_state_corruption_are_detected
1 passed; 0 failed

cargo test -p vmm-core --all-features \
  ram_vcpu_and_gic_corruption_each_fail_before_restore
1 passed; 0 failed

cargo clippy -p snapshot-store --all-targets --features test-utils -- -D warnings
cargo clippy -p vmm-core --all-features --all-targets -- -D warnings
exit status 0 (pre-existing clippy.toml invalid-path notices only)
```

The second oracle seals canonical arm64 state with distinctive core-register and
userspace-GIC timer fields, independently flips a resident RAM-page byte, the
vCPU field, and the GIC field, and requires the matching typed integrity failure
before restore decoding or VM replacement.
- **FAIL — two same-seed archive hashes:** not started.
- **IN PROGRESS — every archived lineage replays byte-for-byte:** durable
  lineage reconstruction is implemented and green for focused snapshots; the
  whole retained archive and real guest remain open.
- **IN PROGRESS — snapshot restore counter and uninterrupted-continuation hash
  oracle:** genesis/continuation counters are now recorded, replay-verified, and
  tamper-evident, and focused restore evidence is green; sampled campaign
  branch-point hashes remain open.
- **IN PROGRESS — in-process/guest/transport cross-build differential:** the
  independent synthetic-ROM chord-endpoint differential is green; the shipped
  guest image and external SMB ROM run remain open.
- **IN PROGRESS — thousands of mid-workload branch/replay cycles:** the bounded
  cache eviction oracle crosses 1,024 stored snapshots, but a real campaign with
  thousands of mid-workload branches has not run.
- **FAIL — altered-chord archive comparator negative:** not started.
- **PASS — RAM, vCPU, and GIC/device stored-snapshot corruption negatives:** all
  three seeded corruptions fail at the retained-store integrity boundary before
  decode or VM replacement.

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
