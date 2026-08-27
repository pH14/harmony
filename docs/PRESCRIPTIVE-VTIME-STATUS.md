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
    absolute guest-clock deadline, PPI27 line level, assertion/ACK counters, and
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
    to its first whole V-ns, filters PPI27 through the same GIC Group-1/enable/
    priority/active gates as real delivery, and lands exactly through
    `IdlePlanner`; post-exit service raises PPI27 at that normalized event.
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
    requested frame. The guest follows `setup_complete` and every
    `frame_complete` with a volatile read of the board pvclock ABI register. That
    already-modeled MMIO exit is the synchronized pre-consume boundary, so a
    continuation snapshot never resumes after consuming or exhausting the next
    input.
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
26. **Every durable remote archive snapshot binds its canonical whole-state
    hash.** The snapshot captures the control server's whole-state hash at the
    same stopped chord boundary as its handle, WRAM, frame, and lineage. Fast
    live-handle restore and genesis-lineage reconstruction both compare the
    immediate restored hash before the target becomes usable. The remote
    checkpoint format advances to v2 because this evidence is required, not an
    optional field that an older checkpoint could silently omit.
27. **The archive comparator negative changes input-visible state, not merely
    serialized metadata.** Its synthetic NROM continuously strobes controller 1
    and writes the A-button bit into the WRAM screen-page byte used by the
    archive key. Two one-worker campaigns use the same seed and policies; the
    negative backend flips that one bit for exactly the first chord. Different
    archive SHA-256 values therefore prove the comparator observes an emulated
    lineage divergence rather than a report-only field.
28. **The HVF control listener creates one fresh server and VM per searcher
    session.** The generic campaign constructs a bootstrap target and then its
    worker target; reusing one disconnected `ControlServer` would lose the
    session-local protocol state and snapshot pool. `hvf_control_server` binds a
    caller-supplied Unix path, accepts those sessions sequentially, and composes
    each live VM and every within-session restore factory from the same image,
    initramfs, boot arguments, RAM size, GIC, pvclock, and control-slot root. It
    never unlinks a pre-existing socket target.
29. **The production control-socket campaign is always a dual-build
    differential.** `ControlSocketSmbBackend` constructs the external guest and
    an independent in-process TetaNES target from the same caller-supplied ROM;
    there is no production switch that can disable comparison. Gameplay genesis,
    every complete chord boundary, and every paired snapshot restore compare full
    2-KiB WRAM plus death, victory, and exit state. Any mismatch aborts the
    campaign instead of becoming an admissible crash result. Durable checkpoints
    retain both snapshot halves and advance to remote format v3. A planted
    component-ROM mismatch and a campaign-level synthetic divergence prove both
    the comparator and fail-loud path can fire before the external-ROM run counts.
30. **The live M2 oracle is one fail-closed, byte-attested operation.**
    `prescriptive-m2-oracle.sh` builds and entitlement-signs the production HVF
    listener and searcher tools, runs the external-ROM `smb-smoke` gate, then
    runs two caller-budgeted one-worker campaigns at the same seed, requires
    byte-identical archives, streams, reports, and snapshot checkpoints, and
    replays the first campaign through a fresh VM. It rejects any false smoke
    invariant, fewer than 2,000 continuation restores, or any absent genesis
    restore. The game-image builder publishes the embedded ROM SHA-256 only after
    a successful initramfs pack; the oracle matches that sidecar to the host ROM
    before launch and writes a manifest binding the kernel, initramfs, ROM,
    signed listener, smoke, campaign, and continuation-oracle executables.
    Missing artifacts, skipped payload markers, watchdogs, panics, partial server
    session counts, or an existing output directory all fail loudly and cannot
    produce the success record.
31. **Real continuation samples come from the retained campaign, not a toy
    lineage.** The live runner selects 32 evenly distributed, non-genesis
    snapshots from the first campaign's complete remote checkpoint. At each
    mid-workload branch point, `smb-vtime-continuation` restores and verifies
    its retained whole-state hash, takes a fresh paired guest/in-process
    snapshot, executes two deterministic vocabulary chords uninterrupted, then
    restores the pair and compares the whole-machine hash after every repeated
    chord. The choices depend only on archive id, not SMB routes or coordinates.
    The success report records every branch id, lineage length, branch hash, and
    chord-hash sequence. A test-only control machine corrupts exactly the first
    replayed chord hash and proves the production comparator rejects index zero.
32. **The standalone workspace carries no ignored unmaintained postcard
    dependency.** The imported searcher and fuzzer requested `postcard`'s default
    heapless feature even though both use its allocated encoding API. Disabling
    that unused default removes `heapless` 0.7 and unmaintained
    `atomic-polyfill` from the all-target graph; the advisory is not ignored.
    The root license policy now documents and admits the actual
    AGPLv3-compatible Zlib, BSL-1.0, and file-scoped MPL-2.0 transitive terms
    brought by the pre-existing LibAFL/TetaNES graph. The resulting full
    `cargo deny check` reports advisories, bans, licenses, and sources all OK.
33. **A control-transport read failure is not an all-zero guest state.** The
    remote production target now caches only WRAM returned by a successful
    checked boundary read. Reset replays genesis and reads the complete mirror
    through one fallible operation before publishing the new observation; on
    either failure the target reports `Crash` and retains the last validated
    bytes. A planted failure on the reset read proves the production path fails
    closed instead of allowing the former zero-filled fallback to reach the
    cross-build comparator.
34. **The native ARM build host is msr1.** The prescriptive contract is
    architecture-based, and the validated host is Linux/aarch64 on a CIX P1
    with Cortex-A520/A720 cores and LSE. Build entry points name that actual
    requirement and validated host.
35. **The M2 game profile requests TMPFS's SHMEM dependency explicitly.** The
    first native msr1 build proved the prior profile could not pass its own
    `CONFIG_TMPFS=y` publication gate because `tinyconfig` had explicitly
    disabled SHMEM. The game fragment now sets `CONFIG_SHMEM=y`, and the kernel
    builder asserts both SHMEM and TMPFS after `olddefconfig`.
36. **FUTEX joins the LSE-only owned-kernel contract.** The corrected game
    profile exposed 14 hard-coded LL/SC instructions in arm64's upstream futex
    user-access helpers even though the ordinary atomic framework was already
    LSE-only. A Harmony-config-only kernel patch uses the corresponding
    acquire-release LSE operations with the same uaccess exception handling and
    final barrier; non-Harmony configurations retain the upstream implementation.
    The raw executable scanner remains the independent publication oracle. On
    msr1 the planted probes were rejected, both `vmlinux` and the vDSO reported
    zero live-counter programs and zero LL/SC instructions, and `Image-game`
    published at 3,209,224 bytes with SHA-256
    `c98c7b660abd550d9de120975132935204c6409799b3b01c33a17341d9d164fc`.
37. **The arm64 control pages use volatile scalar marshalling.** Linux maps the
    low reserved request/response GPAs through `/dev/mem` as device memory. The
    production guest's first catalog call previously died with `SIGBUS` at musl
    `memcpy` (`PC 0x4a99f0`, caller
    `hypercall_proto::Client::exchange_copy` at `0x40a67c`) because the routine
    selected paired AArch64 stores. The same static agent ran natively on `msr1`
    through emulator initialization and failed only later at the expected
    pagemap-PFN privilege check, isolating the device mapping. The transport now
    zeroes and copies shared pages with aligned volatile `u64` accesses plus byte
    tails; ordinary buffers use unaligned scalar accesses. The wire bytes and
    ABI are unchanged. Native tests, strict clippy, and the pinned Miri suite are
    green, and the rebuilt whole image remains clean under the independent
    LL/SC and counter scanners.
38. **Every SDK lifecycle event has an explicit post-doorbell synchronization
    intercept.** A lifecycle doorbell is intentionally unsynchronized, so
    deferring `setup_complete` to the next payload fetch captured the guest after
    it had already received `UnknownService`; doing the same after
    `frame_complete` let an exhaustion request beat the logical-frame deadline.
    The agent maps the board pvclock register frame before setup and, immediately
    after setup and every frame report, reads its ABI register. The read is an
    already-modeled MMIO exit and must return ABI 1. A live trace showed the
    snapshot point on that MMIO exit before every subsequent payload request;
    the focused one-job campaign completed both control sessions with one
    continuation and two genesis restores.
39. **An early chord yield is terminal only when WRAM independently says so.**
    The 2,000-job oracle's first non-setup failure occurred deterministically at
    job 27: both builds reported death, but the guest had executed one extra
    frame because it always finished the hold. The SMB guest observation layer
    now stops at the first death/victory frame, matching the existing search
    target. The socket adapter arms the lifecycle stop explicitly, accounts the
    reported partial frame delta, and accepts it only when the published WRAM
    independently reports death or victory; a committed negative rejects an
    unexplained early yield. A 40-job real-ROM rerun crossed four deaths with 82
    continuation restores and exact guest/in-process agreement.
40. **Campaign length and snapshot churn are independent live gates.** The
    prescriptive criterion requires thousands of branch/replay cycles, not a
    particular job count. A 2,000-job calibration crossed 2,052 continuation
    restores by job 910 but correctly hit the original 1,800-second campaign
    watchdog at job 1,188. The runner therefore accepts any positive explicit
    execution budget and retains the independent hard post-run floor of 2,000
    continuation restores plus at least one genesis restore. The sealed
    900-job run completed 2,031 continuation restores inside the same watchdog;
    reducing the budget cannot make an under-churn run pass.
41. **Empty lineage is gameplay genesis even when a transient handle exists.**
    A live root archive snapshot carried a session-local handle distinct from
    the internally marked genesis handle. Live execution happened to rebuild it
    through durable lineage, while a fresh replay used the transient handle and
    reported two continuation restores instead of one genesis plus one
    continuation restore. Root restoration now always replays the marked
    gameplay-genesis handle before consulting the live-handle cache. A focused
    unit test proves identical class-separated deltas before and after snapshot
    serialization, and the corrected fresh-VM verifier replayed all 900 jobs.
42. **Future milestone validation is claim-based.** Each load-bearing claim gets
    one meaningful positive oracle, one planted negative, and one genuinely
    independent comparator. Broad workspace checks and exhaustive seed sweeps
    belong in CI/nightly unless a result is directly load-bearing for the
    milestone being sealed.
43. **The remaining milestone dependency order is strict.** After sealed M2,
    M3 measures real-payload liveness and performance; M4 completes the ARM64
    KVM backend on msr1; M5 proves bidirectional HVF↔KVM cross-host determinism
    and snapshot portability; and M6 runs the instrumented concurrency-discovery
    measurement. A later milestone does not begin before its predecessor seals.
44. **M3 consumes one immutable initialized PostgreSQL fixture.** Independently
    rebuilding initialized database directories is not an M3 claim. One clean
    static PostgreSQL guest state was initialized, shut down consistently, and
    captured as `initramfs-postgres.cpio.gz` with SHA-256
    `8a6d3a3e1eb5742d790bf53b4010f917ba176e25955aaf0800bca77687dc7720`.
    Every M3 liveness and performance run consumes those exact bytes; the
    fixture must not be rebuilt or substituted while M3 is being validated.
    The discarded rebuild-only work included initialized-cluster
    canonicalization, `pg_resetwal` rewriting, alternate-archive comparison,
    and source changes whose only purpose was byte-identical independent
    initialization. The sole retained PostgreSQL source patch omits optional
    PL/pgSQL during build-time `initdb`, because the required fully static musl
    guest cannot load `plpgsql.so`; it is a payload-executability requirement,
    not a reproducible-initialization claim. Build-time `initdb` is removed from
    the published fixture.
45. **M3 performance evidence is intrinsic to the ARM run.** Phase-separated
    wall time, workload rows/second, exit count/density, bounded V-time gaps,
    watchdog, correctness, and health measure the load-bearing claim under the
    controlled deterministic exit policy. Descriptive-x86 throughput is an
    optional diagnostic only: absence or malformed diagnostic input cannot fail
    M3, and no cross-host slowdown ratio is an M3 threshold.

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
  the deadline, logs PPI27 on the WFI event, passes the placement checker, and
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
  The exact deadline/DISARM/ACK protocol now drives level-triggered PPI27 through
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
  payload digests, one PPI27 placement, 55 interval checkpoints plus the
  `/init` checkpoint, final state hash, and canonical log digest. The harness
  compared the complete normalized text logs byte-for-byte in addition to
  requiring the compact summaries to match.
- **PASS — placement checker green for every boot:** all ten production logs
  passed independently against their deadline schedules with one real PPI27
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

- **PASS — guest payload, control-protocol `Machine` client, and complete live
  campaign oracle.** The native Linux/aarch64 build on `msr1` produced the
  LSE-only game kernel and static musl/TetaNES initramfs. The signed Apple-HVF
  composition booted that image on the M1 Max and completed the external-ROM
  smoke, two same-seed 900-job campaigns, 32 retained continuation samples, and
  a complete 900-job replay through a fresh VM. The production socket target
  compared the guest and independent in-process TetaNES build at genesis, every
  chord boundary, and every restore. All fail-closed gates and planted negatives
  described below remain green; no M3 work began before this result was sealed.

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
4 passed; 0 failed

cargo check --manifest-path harmony-linux/tetanes-agent/Cargo.toml \
  --target aarch64-unknown-linux-gnu --locked
exit status 0

cargo clippy --manifest-path harmony-linux/tetanes-agent/Cargo.toml \
  --all-targets -- -D warnings
exit status 0 (pre-existing clippy.toml invalid-path notice only)

MIRIFLAGS=-Zmiri-permissive-provenance \
  cargo +nightly-2026-06-16 miri test \
  --manifest-path harmony-linux/tetanes-agent/Cargo.toml
2 passed; 0 failed; 2 TetaNES frame tests intentionally ignored under Miri
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
11 passed; 0 failed

cargo test -p searcher --lib smb::campaign::tests
21 passed; 0 failed (706.00 s; includes the 24-seed live/replay sweep)

cargo clippy -p searcher --all-targets -- -D warnings
exit status 0 (pre-existing clippy.toml invalid-path notice only)

cargo fmt --all -- --check
exit status 0

cargo check -p searcher --bin smb-campaign
exit status 0

cargo check -p searcher --bin smb-vtime-continuation
exit status 0

cargo check -p vmm-core --bin hvf_control_server
exit status 0

cargo test -p searcher --lib \
  restore_accounting_is_recorded_replayed_and_tamper_evident
1 passed; 0 failed

cargo test -p searcher --lib \
  archive_hash_comparator_catches_one_altered_chord
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
The continuation oracle snapshots after one chord, records the canonical hash
after every chord of a two-chord uninterrupted suffix, restores the branch,
requires the immediate hash to equal the snapshot, and reproduces the exact
per-chord hash sequence. Flipping one bit of the retained expected hash makes
the same restore fail loudly.
The production checkpoint sampler applies that same uninterrupted/restore/repeat
comparison to 32 evenly spaced, non-genesis retained branch points. Its focused
negative corrupts only the first replayed chord hash and is rejected at chord
index zero; the external-ROM execution below supplies the independent production
evidence rather than being inferred from this portable proof.
The production socket backend now pairs every external guest target with the
independent in-process build, compares full WRAM and terminal state at genesis,
after every chord, and after every paired restore, and makes divergence a hard
campaign error. Its planted component-ROM mismatch reaches `Crash` at the target
boundary, while the separate generic-campaign negative proves that boundary
cannot be serialized as a successful job or report.

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

### M2 live-oracle checkpoint evidence

```text
bash -n harmony-linux/scripts/prescriptive-m2-oracle.sh \
  harmony-linux/linux/build-arm64-game-image.sh
shellcheck harmony-linux/scripts/prescriptive-m2-oracle.sh \
  harmony-linux/linux/build-arm64-game-image.sh
plutil -lint consonance/vmm-backend/hvf.entitlements.plist
exit status 0; plist OK

harmony-linux/scripts/prescriptive-m2-oracle.sh \
  harmony-linux/build/arm64/Image-game \
  harmony-linux/build/arm64/initramfs-game.cpio.gz \
  '/Users/phemberger/Downloads/Super Mario Bros. (World)/Super Mario Bros. (World).nes' \
  harmony-linux/build/arm64/initramfs-game.rom.sha256 \
  /private/tmp/harmony-m2-oracle-final-root-classification 900

M2_CAMPAIGN_ORACLE_OK
campaign_seed=0x5eedca22
execution_budget=900
archive_sha256=b868c1f03d649d4514c21370e5ccb2e0322e52d62fa178b1edc3db7d6feab7c3
archive_entries=1088
genesis_restores=8
continuation_restores=2031
sampled_branch_points=32
continuation_chord_hashes=64
smb_smoke_verified=true
same_seed_archives=2
fresh_vm_replay_verified=true
```

The native msr1 Linux/aarch64 build published Image-game SHA-256
c98c7b660abd550d9de120975132935204c6409799b3b01c33a17341d9d164fc
and initramfs SHA-256
8d674e8f6f7ead55cca0dc1ff2f585a19f0548b529f9e5b8ba1c6209c5907c68.
The embedded and host ROM both matched SHA-256
0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea.
The final whole-image admission scans found no LL/SC instruction and no live
generic-counter access in the kernel, vDSO, BusyBox, musl, or TetaNES agent.

Both live campaigns completed 900 executions, 57,152 frames, 41 deaths, 1,088
retained entries, eight genesis restores, and 2,031 continuation restores. The
runner compared their archive, stream, deterministic report, and complete
snapshot-checkpoint bytes. The retained stream SHA-256 was
b304c47d35a6d8df519c8d7919d9082bf2e3af1a0c34b2b583a2503b5ed6d502;
the fresh-VM replay reproduced report SHA-256
e261549973e28b794ceb686acd51309e0237cd2d2807c763592fd89b29fb48b4.
All smoke, campaign, continuation, and replay stderr artifacts were empty, and
every expected control-server session completed successfully.

### M2 real-ROM and complete standalone-workspace evidence

```text
HARMONY_SMB_ROM='.../Super Mario Bros. (World).nes' \
  cargo run --release --manifest-path dissonance/Cargo.toml \
  -p searcher --bin smb-smoke -- /private/tmp/harmony-m2-smb-smoke-abaf1f7a
exit status 0
same_input_identical_ram_trace=true
snapshot_cache_equivalent=true
headless_ram_trace_equivalent=true
same_seed_campaign_reproducible=true
mini_campaign_corpus_size=5
final_frame_count=360
final_changed_indices=153

cargo build --manifest-path dissonance/Cargo.toml --all-features
exit status 0

cargo nextest run --manifest-path dissonance/Cargo.toml --all-features
96 passed; 0 failed; 0 skipped (826.498 s, including the 24-seed replay sweep)

cargo test --manifest-path dissonance/Cargo.toml -p searcher --lib \
  smb::remote::tests
11 passed; 0 failed (includes reset-read, early-yield, and root-restore negatives)

cargo clippy --manifest-path dissonance/Cargo.toml \
  --all-features --all-targets -- -D warnings
cargo fmt --manifest-path dissonance/Cargo.toml --all -- --check
exit status 0 (pre-existing clippy.toml invalid-path notices only)

cargo tree --manifest-path dissonance/Cargo.toml --target all \
  -i atomic-polyfill
error: package ID specification `atomic-polyfill` did not match any packages

cargo deny --manifest-path dissonance/Cargo.toml check
advisories ok, bans ok, licenses ok, sources ok
```

- **PASS — two same-seed archive hashes:** both 900-job live runs produced archive
  SHA-256 `b868c1f03d649d4514c21370e5ccb2e0322e52d62fa178b1edc3db7d6feab7c3`,
  and the runner also byte-compared their streams, reports, and checkpoints.
- **PASS — every retained campaign job replays byte-for-byte:** a fresh VM
  replayed all 900 recorded jobs, reproduced the archive, stream, report, restore
  counts, frame counts, admission decisions, and checkpoint bytes, and wrote
  `replay_verified=true`.
- **PASS — snapshot restore counter and uninterrupted-continuation hash oracle:**
  both campaigns reported eight genesis and 2,031 continuation restores; replay
  recomputed the same class-separated deltas. All 32 non-genesis retained branch
  samples reproduced their immediate state hash and both repeated chord hashes,
  for 64 matching continuation hashes total.
- **PASS — in-process/guest/transport cross-build differential:** the production
  run compared complete WRAM and terminal state between the shipped guest and
  independent in-process TetaNES build at genesis, every chord, and every paired
  restore. All 900 jobs completed in both same-seed runs and again in fresh replay.
- **PASS — thousands of mid-workload branch/replay cycles:** the live report
  contains 2,031 continuation restores from active retained branch points, above
  the independent 2,000-cycle floor; the 32 sampled points were all non-genesis.
- **PASS — altered-chord archive comparator negative:** the input-sensitive ROM
  and one-chord perturbation produce different archive SHA-256 values at one
  otherwise identical seed and policy configuration.
- **PASS — RAM, vCPU, and GIC/device stored-snapshot corruption negatives:** all
  three seeded corruptions fail at the retained-store integrity boundary before
  decode or VM replacement.

**M2 overall: PASS.** The native LSE-only image, real-ROM smoke, dual-build
differential, two same-seed campaign artifacts, independent restore-count floor,
sampled uninterrupted/restored hashes, complete fresh-VM replay, and all
anti-vacuity negatives are green on the current M2 head. M3 did not begin before
this evidence was recorded.

## M3 — liveness on a real payload

- **PASS — canonical PostgreSQL fixture under prescriptive V-time:** the signed
  Apple-HVF runner booted immutable snapshot
  `8a6d3a3e1eb5742d790bf53b4010f917ba176e25955aaf0800bca77687dc7720`
  with audited kernel
  `91b4f5781c32b01e9d10a7762f7a8951e83d49a9442edd72e8f61f8dc10a72f0`.
  The real static PostgreSQL 17.10 container became ready, completed all 20 SQL
  operations, performed its shutdown checkpoint, and stopped cleanly before
  emitting the logical terminal marker. The host stopped on that marker rather
  than treating the architecture's non-returning halted state as a liveness
  failure.
- **PASS — acceptance, watchdog, and kernel health:** event 101,791 reached
  `ARM64_PG_M3_READY`; the per-entry 5-second watchdog did not fire; all 20 rows
  matched their exact row/count/triangular-sum oracle through `20/20/210`; UUID
  and timestamp shapes were valid; the guest dmesg scan and an independent host
  serial scan found no RCU stall, soft lockup, or watchdog BUG. The planted
  acceptance negatives reject an absent terminal marker and a false final SQL
  aggregate. A separate native run extracted the same fixture bytes and
  independently completed the same 20-row workload and clean shutdown.
- **PASS — bounded inter-exit V-ns gaps:** the normalized trace contained 94,442
  gaps, maximum 10,000,000 V-ns, against the documented two-tick limit of
  20,000,000 V-ns. The independent guest-visible pvclock observation agreed on
  both count and maximum. Planted negatives reject a 20,000,001-V-ns gap and a
  one-unit comparator mismatch.
- **PASS — intrinsic ARM performance report:** the v2 report records boot,
  PostgreSQL startup, ready-to-workload, workload, shutdown, and kernel-health
  phases with wall time, exit count, and integer exit density. The fixed
  20-row workload took 141,161,759,875 host ns (141 milli-rows/second), while
  the whole run took 946,149,572,291 host ns across 101,792 exits. The
  independent normalized trace contains exactly the same 101,792 events.
  Unit positives and planted empty/unordered-phase and exit-count-mismatch
  negatives pass. Optional x86 diagnostic parsing is isolated from the issue
  list; the live report records `NOT_PROVIDED non_blocking=true` and passes.
- **PASS — fail-loud report capability:** the report includes the full histogram,
  intrinsic ARM phases, terminal source, watchdog, kernel health,
  interrupt-placement counters, and normalized-trace digest. Unit tests prove
  it reports excessive gaps, comparator disagreement, missing/unordered phase
  evidence, exit-count disagreement, and malformed workload output instead of
  producing a vacuous pass.

Final clean bounded report:

```text
/private/tmp/harmony-m3-live/m3-hvf-intrinsic-v2-clean-cap105000.report
sha256 997f325d00fe8ef44be4bbef431e38812dd2fe281e81d5f9c3ae4fa1b37bf80c
format consonance.prescriptive-m3-report.v2; status PASS
terminal_event 101791; terminal_source ARM64_PG_M3_READY
watchdog PASS; acceptance rows=20 PASS; kernel_health PASS
clockevents deliveries=12022 placement_status=PASS
gap_result count=94442 max_vns=10000000 status=PASS
independent_pvclock gaps=94442 max_vns=10000000 status=PASS
performance_intrinsic total_wall_ns=946149572291 total_exits=101792 PASS
phase boot_to_postgres_start: wall_ns=276678465041 exits=21774
phase postgres_startup:       wall_ns=400870293125 exits=36225
phase ready_to_workload:      wall_ns=80401705709  exits=12954
phase workload:               wall_ns=141161759875 exits=23179
phase postgres_shutdown:      wall_ns=32964387208  exits=5435
phase kernel_health:          wall_ns=14072961333  exits=2225
workload_rate rows=20 wall_ns=141161759875 milli_rows_per_second=141
exit_count_comparator event_loop=101792 normalized_trace=101792 PASS
optional_x86_diagnostic NOT_PROVIDED non_blocking=true
```

The live run also corrected two load-bearing implementation defects rather than
weakening the oracle: the generic clockevent is PPI27, matching the DT and Linux
handler, and HVF now records interrupt acceptance only on the trapped
`ICC_IAR1_EL1` read instead of merely observing unmasked PSTATE before an
unrelated exit. The kernel advances the fixed 10-ms execution tick at syscall,
context-switch, and idle-poll seams; the latter was localized by an earlier
watchdog report at `cpu_idle_poll`. Focused positive and wrong-register/
direction/absent-IRQ negatives cover the acceptance fix.

Focused and safety gates on the M3 implementation:

```text
cargo test -p vmm-core m3_report --all-features
11 passed (positive acceptance/gap/performance plus all planted negatives)
cargo test -p vmm-core --test arm64_skeleton --all-features
21 passed
cargo test -p vmm-backend pending_irq_is_accepted_only_at_the_guest_iar_read \
  --all-features
1 passed
MIRIFLAGS=-Zmiri-permissive-provenance \
  cargo +nightly-2026-06-16 miri test -p vmm-backend --all-features
58 unit + 3 contract + 2 dynamic + 2 exhaustive + 20 run-loop + 1 vCPU-state passed
cargo nextest run --all-features (pre-push hook)
1277 passed, 25 skipped
cargo clippy --all-features --all-targets -- -D warnings
exit status 0 (pre-existing clippy.toml invalid-path notices only)
cargo fmt --all -- --check
exit status 0
```

**M3 overall: PASS.** The immutable real-payload fixture, correctness,
watchdog, kernel-health, interrupt-placement, phase-separated intrinsic ARM
performance, bounded-gap, planted-negative, independent pvclock, and independent
exit-count claims are green. The absent optional x86 diagnostic is explicitly
non-blocking. M3 is sealed; no M4 work began before this result was recorded.

## M4 — complete the ARM64 KVM backend on msr1

- **FAIL — KVM/arm64 interrupt-delivery implementation:** not started.
- **FAIL — M1 boot oracle on the byte-identical image:** not started.
- **FAIL — GIC save/restore positive, planted negative, and independent
  comparator:** not started.
- **FAIL — honest backend capability publication:** not started.

## M5 — bidirectional HVF↔KVM determinism and snapshot portability

- **FAIL — byte-attested bidirectional same-seed normalized logs:** not started.
- **FAIL — immediate cross-host restore `state_hash` equality:** not started.
- **FAIL — both-direction uninterrupted continuation comparison:** not started.
- **FAIL — planted cross-host increment mismatch:** not started.
- **FAIL — independent architectural-state comparator:** not started.

## M6 — instrumented concurrency-discovery measurement

- **FAIL — SDK threshold protocol:** not started.
- **FAIL — deliberately racy Go/Rust suite with known schedules:** not started.
- **FAIL — deterministic seeded reproduction:** not started.
- **FAIL — held-out schedule discovery within predeclared budgets:** not started.
- **FAIL — wrong-schedule negative for every entry:** not started.
- **FAIL — held-out schedules absent from seeds/fixtures and per-bug report:** not
  started.

## Repository-wide final gates

- **PASS at M3 — `cargo build --all-features`:** exit status 0.
- **PASS at M3 — `cargo nextest run --all-features`:** 1277 passed, 25 skipped
  in the pre-push gate.
- **PASS at M3 — `cargo clippy --all-features --all-targets -- -D warnings`:**
  exit status 0 (the pre-existing invalid-path notices from `clippy.toml` remain
  non-fatal).
- **PASS at M3 — `cargo fmt --all -- --check`:** exit status 0.
- **PASS at M3 — `cargo deny check`:** advisories, bans, licenses, and sources all
  green.
- **PASS at M3 — changed unsafe crate Miri:** the pinned vmm-backend suite passed
  every platform-neutral unit and integration test under permissive provenance.
- **PASS at M2 — standalone search workspace:** build, strict clippy, format,
  deny, and 96-test nextest suite are green; the full seed sweep remains a
  CI/nightly concern after this directly load-bearing M2 record.
- **PASS at M2 — unsafe-crate Miri:** pinned Miri passed the hypercall-doorbell
  loopback suite and the agent's pure tests; full-frame agent tests are explicitly
  excluded only under Miri.
- **PASS at M2 — native Linux/aarch64 validation on msr1:** exact-source
  hypercall-doorbell and agent tests plus strict clippy passed, and published
  image hashes matched the local build inputs and outputs.
- **PASS at M0 — Linux-frozen `vmm-core` public API:** exact cross-target match.
- **PASS at M0 — coverage ratchet:** 94.76% workspace region coverage against the
  workflow's 90% floor; the new module measures 90.08%.
- **PASS at M0 — mutation gate:** all 53 changed-code mutants accounted for; no
  survivors or timeouts.
- **PASS at M0 — aarch64 architecture seam:** full all-feature/all-target clippy
  for `aarch64-unknown-linux-gnu`, exit status 0.
- **FAIL — Kani and remaining final quality-toolchain gates:** not yet run for the
  complete plan.

## x86 — VM-exit-count V-time on GitHub-hosted runners

Branch `claude/x86-prescriptive-vtime`, forked from `a51fe015`. Substrate: the
standard GitHub-hosted `ubuntu-24.04` x86-64 runner (4 vCPU, Azure, nested
virtualization; hardware alternates between Intel and AMD draws per job).
Milestones X0–X3 apply the §3 oracle discipline to the stock-KVM x86 backend.
`.github/workflows/x86-vtime.yml` runs six probe replicas per push — one push
samples both vendors — plus a Linux fmt/clippy/test job for the touched crates,
which the macOS development host cannot check. Every result is uploaded as an
artifact because the runner is unreachable after the job ends. The repository
and its Actions logs and artifacts are public: no workflow, test, or artifact
on this branch may contain, fetch, or require a NES ROM.

### Recorded decisions (x86)

1. **Milestones are named X0–X3** to stay distinct from this document's ARM
   M-milestones: X0 runner probe, X1 minimal guest deterministic on the runner,
   X2 Linux boots to `/init` deterministically, X3 cross-vendor identity
   (Intel and AMD draws produce byte-identical normalized logs).
2. **Rust 1.98's `clippy::chunks_exact_to_as_chunks` fired at ten
   constant-size call sites across the shared workspace**, so the branch's
   Linux clippy check could not run `-D warnings`. All ten are converted to
   `as_chunks`/`as_chunks_mut` (commits `0565a2cb`, `70c28e5c`), verified with
   `cargo +1.98.0 clippy --workspace --all-features --all-targets -- -D
   warnings`. Main carries the same latent failure: issue #197.
3. **X0's planted negative sets the `/dev/kvm` pre-state explicitly**
   (`chmod 600`) before expecting the probe to fail closed, because the runner
   image's default device mode is not documented and a permissive default
   would make the negative vacuous.
4. **X1 delivery reuses the ruled userspace-xAPIC posture.** `KvmBackend`
   creates no in-kernel irqchip (`KVM_IRQCHIP_NONE`, the R1 ruling recorded in
   `kvm_sys.rs`); the prescriptive `deliver` callback injects through
   `Injection::Interrupt` → `KVM_INTERRUPT`, and `plan_irq_entry`'s
   interrupt-window handshake defers entry until the guest is interruptible.
   No new delivery mechanism is introduced for X1.
5. **The prescriptive Linux composition skips the §1.1 `det-cfl-v1` host
   gate.** `boot_linux_stock_prescriptive` composes via `compose_linux`
   without `hostassert::enforce()`: that baseline freezes one physical CPU for
   the descriptive determinism claim, while this model's claim is defined over
   the exit stream plus the frozen CPUID/MSR contract and is exercised on
   heterogeneous commodity hosts. Residual native-behavior divergence is
   exactly what the X2/X3 gates measure.
6. **The x86 prescriptive per-class durations are the arm64 row values**
   (`vendor::x86::contract`: interrupt-controller 1000, serial 2000,
   paravirtual 1000, trapped time read 1, architectural control 1000),
   carried over unchanged pending an x86-specific ruling. Only the assignment
   structure — one constant per class, exactly once per classified exit — is
   contractual today.
7. **x86 arch-exit normalized payloads carry a leading variant discriminant
   byte.** `X86Exit` has eight variants and several share a normalized class
   (`Rdtsc`/`Rdtscp`; `Rdrand`/`Rdseed`); the discriminant keeps any two
   variants from aliasing byte-for-byte. arm64's single-variant arch exit
   needs none, so its payload shape is unchanged.
8. **V-time intercepts read `VtimeWiring::intercept_work()`** — the live
   counter on the descriptive path, constant zero on the prescriptive path
   (the whole clock lives in `vns_base`). This is what lets the
   `emulate-vtime` TSC MSRs, which Linux reads early in boot, be serviced on
   the prescriptive stock path without a hardware work counter. Descriptive
   behavior is byte-identical.
9. **X2 runs measure-first.** The smoke tier (one prescriptive stock boot to
   `/init` and a clean terminal) is the workflow gate; the determinism tier
   (same-seed boots → one normalized log) runs informationally at three boots
   until the smoke tier is stable, then goes to ten as the gate. Known
   remaining for the seal at the time of this decision: the Vmm-path trace
   recorded no interrupt schedule on x86, so delivery placement rode the
   state-hash/vns comparison. Decision 17 wires the LAPIC timer into the
   schedule oracle.
10. **The counter-opcode scan gets a per-toolchain site baseline.** The
   reviewed `rdtsc-allowlist.txt` pins `symbol+offset` sites from the box
   toolchain; offsets and inlining are toolchain-dependent, so the runner
   build (ubuntu-24.04 default gcc 13) fails the exact-accounting scan with
   the same audited kernel. `rdtsc-allowlist-gha.txt` is the runner
   toolchain's captured baseline — 115 sites, verified at capture to be a
   strict subset of the reviewed list at function granularity (no new
   function carries a counter read; justifications stay with the reviewed
   list). `build-kernel.sh` selects the list via `HARMONY_RDTSC_ALLOWLIST`
   (default unchanged: the box list).

11. **The prescriptive x86 composition offers the task-110 clock page.**
   `boot_linux_stock_prescriptive` calls `enable_pvclock(1)`, mirroring the
   arm64 prescriptive compositions. The engine's prescriptive registration
   semantics were already in place: with prescriptive wiring, a pending
   registration arms at the doorbell exit itself (`pvclock_refresh` skips the
   descriptive r17 RDTSC-handshake requirement, which stock KVM can never
   satisfy), the page re-stamps at serviced-exit tails from
   `guest_clock(work = 0)` — the assigned V-time — and the Δ forced-refresh
   deadline stays unarmed. The guest opts in with the `harmony_pvclock`
   cmdline token, now part of the X2 command line.

12. **Guest patch 0001 gains an assigned-at-exit-host mode.** Driven by the
   X2 localizer: the divergent terminal state was randomized userspace
   layout (fs base, CR2, stack/mmap/brk pointers in ~420 RAM pages — CRNG
   outputs) plus intermittently the printk timestamps (sched_clock), all
   fed by the audited native TSC reads that stock KVM cannot intercept.
   With `harmony_pvclock` requested the kernel now leaves the raw TSC
   entirely: `__use_tsc` never enables (sched_clock holds the jiffies
   fallback until the page routes it), `clocksource_tsc_early` never
   registers (and the existing `mark_tsc_unstable` at registration keeps
   the refined tsc clocksource out), and `random_get_entropy()` reads the
   clock page — zero before the page is live, never a raw counter. The
   registration flow is unchanged: on stock hosts the two deliberate
   rdtscs execute natively with their values discarded, and the OUT-armed
   stamp is already on the page when the driver validates it.

13. **The decision-12 amendment re-baselines the runner scan.** Changing
   patch 0001 shifts counter-opcode offsets (and the entropy sites), so
   `rdtsc-allowlist-gha.txt` is re-captured from the runner build under the
   same function-subset review as decision 10. The box-toolchain
   `rdtsc-allowlist.txt` goes stale for box builds of this branch's tree
   and needs a box re-baseline before any merge.

14. **The jitter RNG declines to run under `harmony_pvclock`.** The first
   decision-12 boots hung deterministically with the last serial line at
   fs/9p (runs 33070782691, both replicas byte-identical), and an
   `initcall_debug` boot (run 33080063285) named the site: `jent_mod_init`
   is called and never returns. Cause: `jent_entropy_collector_alloc`
   primes its data pad through `jent_gen_entropy` before any timer sanity
   check runs; with every guest time source a function of assigned V-time,
   time is frozen between VM exits, every jitter delta is zero, every
   measurement is stuck and retried, and the only loop break —
   `jent_health_failure` — returns 0 outside FIPS mode. The guest makes no
   exits in that loop, so V-time stays frozen and the spin is permanent.
   Patch 0001 now has `jent_mod_init` return `-ENODEV` (one deterministic
   pr_info line) when `harmony_pvclock` is active: an assigned clock
   carries no physical timing jitter, so declining is the honest behavior,
   and the DRBG continues without the source outside FIPS mode
   (`drbg_prepare_hrng`). The hunk adds no counter reads and shifts no
   in-function offsets, so the decision-13 allowlist stands. This is the
   first instance of the general class "exit-free guest spin on frozen
   V-time"; the external per-tier `timeout` wrappers added alongside bound
   any future instance to minutes instead of the job cap.

15. **The stock prescriptive composition hides the hardware-RNG CPUID
   bits.** With decision 14 in place the boot reaches `/init` with identical
   step counts (35413) and matching serial on every boot (run 33081862334),
   but tier-2 still diverged at the terminal 256-event `StateHash`: the
   localizer shows the pre-decision-12 userspace-ASLR signature again
   (`FS_BASE`, userspace `CR2`, `CR3` one page, 309 user-space RAM pages;
   serial and all vtim components MATCH). Remaining feed: the frozen
   contract model exposes RDRAND (CPUID.1:ECX[30]) and RDSEED
   (CPUID.7.0:EBX[18]) as exposed-but-trapped, a justification §2 itself
   caveats as requiring the VMX exiting controls stock KVM never surfaces —
   so the instructions executed natively and fed true entropy into the CRNG
   at every reseed. `boot_linux_stock_prescriptive` now installs
   `cpuid_model_hw_rng_hidden()` — the frozen model with exactly those two
   bits cleared — via a CPUID-model parameter on `compose_linux`; the
   descriptive substrates keep the unmodified model. Host-side only, so the
   goal doc's RDRAND stop condition (guest changes beyond the patch set) is
   not triggered; the pinned kernel reaches both instructions only through
   `cpu_feature_enabled` checks on these bits. The frozen §2 table is
   unchanged; the variant is recorded here.

16. **The boot CPU's cyc2ns scale is seeded from cycle 0 under
   `harmony_pvclock`.** With decision 15 in place the localizer narrowed to
   ONE differing RAM page (run 33084120303, gpa `0xf81c000+0xa80` on both
   replicas): two copies of `{cyc2ns_mul=0x40000000, cyc2ns_shift=31,
   cyc2ns_offset}` plus a seqcount of 2 — tsc.c's per-cpu
   `struct cyc2ns` for the frozen 2.0 GHz TSC, with the offset varying per
   boot around −0.23 s. Cause: `cyc2ns_init_boot_cpu` seeds
   `__set_cyc2ns_scale` with a native `rdtsc()`, so the offset is
   `−tsc_now/2` ns — host boot latency — written into per-cpu state even
   though `__use_tsc` never enables and nothing converts cycles at runtime.
   This also explains the tier-2 divergence at the first checkpoint (event
   255): the seeding runs in `tsc_early_init`. Patch 0001 now seeds with
   cycle 0 when `harmony_pvclock` is requested, making the scale data a
   pure function of `tsc_khz`; the unreachable re-seeding callers (cpufreq
   notifier, suspend resume) are unchanged. The runner allowlist needs
   another re-baseline for the shifted tsc.c site (the scan names it).

17. **The LAPIC timer feeds the trace's schedule oracle.** This closes the
   decision-9 remaining item. A LAPIC MMIO write that changes
   `next_timer_deadline()` records a schedule (deadline V-ns, timer vector)
   in the prescriptive trace; a write that disarms the timer records a
   cancel. The x86 vendor post-exit step advances the LAPIC to the event's
   assigned V-time and records any fire as a delivery inside that same
   event, which is what `check_delivery_placement` requires: each delivery
   at the first event whose post-advance V-time covers its deadline, armed
   strictly earlier. Unit tests cover the one-shot fire and the
   disarm-cancels-schedule path against a mock backend. The X2 boot test
   now computes the placement verdict per boot, prints it in the report,
   and asserts it in both the smoke and determinism tiers.

18. **An intermittent divergence is localized by re-drawing bounded
   boots.** The Intel draw of run 33088504233 diverges on 4 of 10 boots at
   the first checkpoint while the exit stream stays identical, so a
   single localizer pair sampled at terminal can land on two agreeing
   boots and see nothing. `x2_component_diff_first_checkpoint` stops
   same-seed boots just past the first checkpoint (~320 steps, sub-second
   each), re-boots until a checkpoint hash differs from the reference
   boot's, and prints the component and byte diff near the divergence
   origin. Both localizer steps run under `if: always()`: they are
   diagnostics, and a determinism-tier failure is exactly when their
   output matters. The x2 matrix goes to four replicas per push because
   the runner pool draws Intel rarely and the open divergence is
   Intel-only.

19. **The XSAVE image is canonicalized to init-compressed x87/SSE at the
   save boundary.** The decision-18 localizer caught the divergent pair on
   the Xeon 8573C draw of run 33095087969: the two 4096-byte images differ
   in exactly one word — `XSTATE_BV` flips between 0x3 and 0x2 — with
   `XCOMP_BV` zero both ways and every state byte identical, including the
   x87 area. XSAVE's init optimization gives one guest-visible state two
   encodings (component present with the init values in its area, or
   component absent with the area ignored), and which one hardware writes
   varies with host scheduling rather than guest behavior: Ice Lake draws
   (8370C, 120 bounded boots across two hosts) never flip, Emerald Rapids
   (8573C) flips on roughly 4 of 10 boots. `canonicalize_xsave`
   (vmm-backend `arch::x86::state`, unit-tested portably) collapses both
   encodings: an x87 or SSE component whose area holds the architectural
   init values gets its `XSTATE_BV` bit cleared, and a cleared component's
   ignored area bytes are set to those init values. `MXCSR_MASK` (a host
   capability constant) and compacted-format images are untouched. Applied
   in `save_xsave`, so the state hash, the component breakdown, and
   snapshots all see one form. The ARM precedent is the BTYPE
   canonicalization: state the architecture calls absent leaves no host
   residue in the saved record.

20. **Every hardware-RNG instruction site in the image is enumerated and
   feature-gated.** The counter-opcode scan gained a second mnemonic class
   (rdrand `0f c7 /6`, rdseed `0f c7 /7`) with its own per-site allowlist
   and self-test fixtures, including the modrm decode that separates those
   forms from cmpxchg8b (`/1`). The justification regime is the opposite
   of the counter class: a counter read is survivable-by-trap, but SVM
   cannot intercept RDRAND/RDSEED, so a reachable site is a true-entropy
   hole; a site is allowlistable only when it sits behind the
   X86_FEATURE_RDRAND / X86_FEATURE_RDSEED check that the decision-15
   CPUID hiding turns off. The runner capture (run 33099482655) names five
   sites — `extract_entropy` (2), `random_init_early` (2),
   `x86_init_rdrand` (1) — each confirmed gated in source (archrandom.h's
   `static_cpu_has` inlines; rdrand.c's `cpu_has` early return). The
   setup/decompressor raw-byte scan covers the same opcodes with zero
   allowance (`CONFIG_RANDOMIZE_BASE` is off, so the decompressor carries
   no KASLR rdrand). The box-toolchain list ships unarmed until a box
   capture exists (issue #199). This is the goal doc's X3 RDRAND
   prohibition audit.

21. **The §2.4 disposition table for the x86 stock prescriptive
   composition.** Each untrusted-instruction channel of
   `docs/VM-EXIT-COUNT-VTIME.md` §2.4, its closure layer, and the
   disposition on stock KVM:

   | Channel | Instructions | Layer | Disposition |
   |---|---|---|---|
   | Identity | `CPUID` | 1 | Intercepted unconditionally on both vendors (architectural on VMX; SVM arms the intercept at vcpu init). Every leaf is answered from the frozen §2 `det-cfl-v1` model with the decision-15 hardware-RNG variant; the model's other hidden features (MONITOR/MWAIT, PMU leaf 0xA v0, PT, ARCH_LBR) ride the same trap. Identical on both vendors by construction. |
   | Time | `RDTSC`, `RDTSCP` | 1 + 2 | Stock KVM leaves both native, so the raw value is host-real; the closure is that no guest code consults it. Patch 0001 under `harmony_pvclock` leaves the raw TSC entirely (decisions 12, 16), and the exact-accounting scan enumerates every image site against the reviewed allowlist (decisions 10, 13). Layer 3 (`CR4.TSD`) is not engaged: the only userspace is the image's own static busybox `/init`. |
   | Machine measurement | `RDPMC` | 1 + 4 | The frozen model's leaf 0xA is version 0, so conforming code never issues it, and stock KVM intercepts it unconditionally on both vendors (VMX: `CPU_BASED_RDPMC_EXITING` is in KVM's required exec-control set; SVM: `INTERCEPT_RDPMC` at vcpu init), emulating against the empty vPMU. |
   | Entropy | `RDRAND`, `RDSEED` | 1 + 2, residual | The §2.4 named residual: SVM cannot intercept them, stock KVM never arms the VMX exiting controls, and there is no user-mode disable. Decision-15 CPUID hiding makes every feature-gated site dead code; the decision-20 scan proves every image site is feature-gated. The residual — an unaudited binary ignoring the pinned feature bits — does not exist on X-milestone workloads (the initramfs is the pinned static busybox) and falls to the cooperative posture in general. |
   | Identity | `MXCSR_MASK` (the `FXSAVE`/`XSAVE` image byte) | pin at save | Uninterceptable and vendor-distinct; decision 22. |

   The initramfs binaries (busybox, libvoidstar) are built from pinned
   sources in the image bake but are outside the opcode scan today; the
   scan covers the kernel proper plus the setup and decompressor stubs.
   Extending the scan over the initramfs is issue #200.

22. **`MXCSR_MASK` is pinned in the saved XSAVE image.** The first
   both-vendor log pair (run 33098100923: one 8573C draw, AMD 7763 and
   9V74 draws) shows the X2 event streams already byte-identical across
   vendors — every `EVENT` line matches, class, payload digest,
   `vns_after`, interrupts — with the state hash diverging in exactly
   three components: `xsave-legacy`, `segments`, and RAM. The
   `xsave-legacy` cause is `MXCSR_MASK` at legacy-area offset 28: AMD
   writes `0x2FFFF` (bit 17, misaligned SSE), Intel `0xFFFF`. The
   contract already rules the field (`docs/CPU-MSR-CONTRACT.md` §2,
   "FPU/XSAVE save-image determinism"): pinned `0x0000FFFF`, asserted on
   the host at VM start. The runner pool spans both vendors and decision 5
   skips the host assert, so the pin moves to the save boundary:
   `canonicalize_xsave` writes the contract value into the image field,
   which `FXRSTOR`/`XRSTOR` ignore on restore. The guest kernel's own
   `FXSAVE` at FPU init still reads the host value into guest RAM
   (`mxcsr_feature_mask` and its `init_fpstate` copy); that is part of the
   open RAM component, measured next by the per-page fingerprint dump.
   The `segments` component (vendor-distinct hidden attributes, the
   SYSRET class) is also open pending the same dump.

## X0 — runner probe

### Build criteria

- `x86_kvm_probe` (`consonance/vmm-backend/src/bin/`): CPU identity, `/dev/kvm`
  access, KVM API version, a 22-entry `KVM_CHECK_EXTENSION` table, then one
  real-mode guest (`OUT` then `HLT`) through the public `KvmBackend`,
  reported as `KEY=VALUE` lines with a single `PROBE=PASS`/`PROBE=FAIL`
  verdict that fails closed.
- The `x86-vtime` workflow above.

### Passes-when criteria

- The probe is green on at least one Intel draw and one AMD draw.
- The planted negative fails closed with `FAIL_REASON=kvm_open` on every
  replica before access is granted.
- The independent comparator — the pre-existing four-test live `kvm_smoke`
  suite, written for the retired determinism box — passes against the same
  hosts.

### X0 command evidence

- Run 33027653620 (commit `6fe4d5e8`): 6/6 replicas `PROBE=PASS`,
  `KVM_API_VERSION=12`, kernel `6.17.0-1022-azure`. Capability table on every
  draw: `X86_USER_SPACE_MSR=1`, `X86_MSR_FILTER=1`, `XSAVE2=4096`,
  `TSC_CONTROL=1`, `TSC_DEADLINE_TIMER=1`, `SPLIT_IRQCHIP=1`,
  `IMMEDIATE_EXIT=1`, `X86_DETERMINISTIC_INTERCEPTS=0` (stock),
  `X86_NOTIFY_VMEXIT=0`.
- Run 33028194843 (commit `70c28e5c`): planted negative green on all six
  replicas (mode 600 → probe exits 1 with `FAIL_REASON=kvm_open`), then the
  udev grant → `PROBE=PASS`; `check` job green (fmt, clippy `-D warnings` on
  Rust 1.98, `cargo test -p vmm-backend --all-features`).
- `kvm_smoke` (`--ignored`, single-threaded): 4 passed / 0 failed on all
  twelve replicas across both runs — bring-up `OUT`+`HLT`, save/restore
  fixpoint over the XSAVE2 image, loud MSR filter with real-mode `#GP`
  delivery, honest capabilities. First pass of this suite off the retired
  box, first under nested virtualization, first on AMD.
- CPU draws observed: AMD EPYC 7763 (Milan), AMD EPYC 9V74 (Genoa), Intel
  Xeon Platinum 8370C (Ice Lake), Intel Xeon Platinum 8573C (Emerald
  Rapids), Intel Xeon 6973P-C (Granite Rapids).

X0 is PASS.

## X1 — minimal guest, deterministic on the runner

### Build criteria

- `consonance/vmm-core/tests/x86_kvm_prescriptive.rs`: `PrescriptiveRunLoop`
  over the public stock `KvmBackend`. The real-mode guest's doorbell `OUT`
  values carry the prescribed durations `[3, 4, 5, 6, 7]`; two deadlines
  (V-time 5 and 15) are scheduled as vector `0x20` and delivered through the
  guest IVT to a handler that increments an in-guest witness counter; one
  poweroff-port `OUT` is the terminal event. Classification fails closed on
  any unmodeled exit.
- Delivery is recorded decision 4: userspace xAPIC, `KVM_INTERRUPT`,
  interrupt-window handshake — no new mechanism.
- Workflow job `x1-minimal-guest`, two replicas per push, reports uploaded
  as artifacts.

### X1 command evidence

- Run 33060911869 (commit `93e29734`), both replicas 3/3:
  - `x1_ten_same_seed_runs_produce_one_normalized_log`: ten same-seed runs
    produce one normalized log — pairwise `compare_normalized_logs` equality,
    equal digests, delivery-placement checker green over every run's log,
    `X1_GUEST_DELIVERIES=2` (both deadlines landed in-guest through the IVT),
    deliveries at exactly events 1 and 3, `X1_EVENTS=6`.
  - Comparator negative on this workload: a one-exit-late delivery is caught
    at event 1, `LogField::Interrupts`.
  - Placement negative on this workload: consistently-late twins compare
    equal but fail `check_delivery_placement` with
    `WrongDelivery { event_index: 1 }`.
- Honest surface: `kvm_smoke::capabilities_are_honest` green on all six of
  the same run's probe replicas.
- Observation, recorded as X3 input: the two X1 replicas drew different
  parts — AMD EPYC 7763 (Milan) and AMD EPYC 9V74 (Genoa) — and produced the
  same digest `33f24b4411ea60ffb753b183859e9263559092802a20710f36e067cebeba1cb7`,
  `state_hash` values included. Cross-host identity held across models within
  one vendor before any §2.4 disposition work; the Intel/AMD pair remains
  X3's claim.

X1 is PASS.

## X2 — Linux boots to /init deterministically on the runner

### Build criteria

- x86 vendor prescriptive wiring (commit `a282c83c`):
  `normalize_prescriptive_exit_x86` + the `Vendor` hook, per-class assigned
  advancement in the port/MMIO/MSR/CPUID dispatch handlers, the per-class
  duration constants (recorded decision 6), and the
  `boot_linux_stock_prescriptive` composition root (recorded decision 5).
- `consonance/vmm-core/tests/x86_kvm_linux_prescriptive.rs`: the smoke tier
  boots the committed image once under prescriptive V-time on stock KVM and
  asserts userspace + a clean terminal; the determinism tier compares N
  same-seed boots' normalized logs and prints the first divergent event with
  a surrounding window from both runs.
- Workflow jobs: `guest-image` builds and caches the bzImage + initramfs on
  the runner (commit `1ff852e9`); `x2-linux-prescriptive` restores the cache
  and runs both tiers in release (the trace's 256-event checkpoints hash the
  full 256 MiB RAM).

### Passes-when criteria (goal doc X2)

- Ten same-seed boots to `/init` produce one normalized log: identical event
  classes/payloads, identical assigned V-time, identical checkpoint state
  hashes, no watchdog, on at least one Intel draw and one AMD draw.
- The expected initial divergence sources are the audited stock-KVM counter
  reads (`harmony-linux/linux/rdtsc-allowlist.txt`: `native_sched_clock`,
  `random_init`, `try_to_generate_entropy`, `ret_from_fork` classes execute
  natively); the measurement tiers exist to rank them before closure work.

### X2 measurement (runs 33086789801, 33088504233)

The first measurement rounds (runs 33064621195, 33066193514) found the
exit stream already deterministic — identical event classes, payload
digests, assigned V-time, and interrupt counts on every boot — with only
the checkpoint state hash divergent, first at event 255. The localizer's
component and byte dumps then named three state sources in sequence, each
closed by a recorded decision: the jitter RNG's unbounded priming spin on
frozen time (decision 14), natively executed RDRAND/RDSEED feeding the
CRNG (decision 15), and the boot CPU's cyc2ns scale seeded from a native
`rdtsc()` (decision 16).

After the third fix, run 33086789801 (3 same-seed boots + localizer, both
replicas): every boot reaches userspace in exactly 35313 steps / 35314
events, final assigned V-time 71621003 vns, `X2_DIVERGENCES=0`, all 25
localizer components MATCH, delivery placement OK on every boot. The
full-log digest `c3bae072…` is byte-identical across the two replicas'
CPU models (EPYC 7763 Milan, EPYC 9V74 Genoa), so the normalized log
already crosses AMD generations.

Run 33088504233 (ten boots as the gate, decision 9's second stage): the
EPYC 7763 replica passes 10/10 with one digest. The first Intel draw of
the whole program (Xeon Platinum 8573C Emerald Rapids) fails 4 of 10
boots. The exit stream is identical on all ten (35314 events, placement
OK, same final V-time); the checkpoint state hash diverges first at event
255, and all four divergent boots carry the same alternative hash there —
a binary divergence at the first checkpoint — while their later
checkpoints take three distinct value paths. Decision 18 records the
localizer built for it and decision 19 the measured cause (an XSAVE
init-optimization encoding flip) and its canonicalization.

Run 33096707232 (decision-19 canonicalization in place): the gate matrix
drew both vendors — two EPYC 9V74 and two Xeon 8573C replicas — and all
four passed: smoke, ten same-seed boots with `X2_DIVERGENCES=0`, delivery
placement OK on every boot, terminal localizer all-MATCH. Each host's
twelve boots (smoke, the ten gate boots, the localizer pair) produce one
digest: `670b8e9c…` on both AMD hosts, `b91a5d19…` on both Intel hosts,
with 35314 events and the same final assigned V-time everywhere. The
hunt job's bounded first-checkpoint draws stayed clean on every replica.
The two vendor digests differ from each other; naming the divergent
field is X3's opening measurement.

X2 is PASS.
