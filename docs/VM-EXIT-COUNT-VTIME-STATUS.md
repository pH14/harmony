# VM-exit-count V-time implementation status

Branch: `claude/consonance-virtual-time-6kvrz6`

This is the live evidence ledger for `docs/VM-EXIT-COUNT-VTIME.md`. A criterion is
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
    `OSLAR_EL1`. The contract accepts only Linux's deterministic zero unlock and
    rejects reads or nonzero writes. M1 initially assigned these traps an
    architectural-control duration; M5's first full cross-host trace proved that
    stock KVM services the same guest instructions in-kernel. They are therefore
    retained in HVF's raw diagnostic log but consume no normalized ordinal and no
    portable V-time. A planted classification negative rejects leaking either
    trap into the portable log. Retained debug registers remain the only stateful
    debug surface.
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
46. **PSTATE.TCO is canonical zero for the no-MTE guest identity.** The frozen
    ARM identity advertises `ID_AA64PFR1_EL1.MTE=0`, and the admitted kernel has
    `CONFIG_ARM64_MTE=n`. The M5 component comparator localized the first
    cross-host state difference to bit 25 alone: physical KVM exception entry
    set `PSR_TCO_BIT`, while HVF left it clear. Backend save clears that
    unsupported residue and restore rejects non-canonical input. Linux also
    clears TCO from `SPSR_EL1` before publishing an exception frame, because a
    nested exception had otherwise copied the host-only bit into one
    `struct pt_regs` word in retained RAM. Focused fake-KVM tests prove both the
    positive round trip and planted TCO rejection; the whole-image kernel build
    remains independently gated for no MTE, no live counter reads, and no LL/SC.
47. **PSTATE.BTYPE is canonical zero for the no-BTI guest identity.** The frozen
    ARM identity advertises `ID_AA64PFR1_EL1=0`, and the admitted game kernel has
    `CONFIG_ARM64_BTI=n`. The M5 game-state comparator localized the remaining
    cross-host difference to `SPSR_EL1` bit `0x800` alone. The only RAM
    differences were two guest exception-frame copies of that exact value, at
    offsets `0x6ace11` and `0x6acfc9`; there was no independent data
    divergence. Backend save now clears both BTYPE bits, restore rejects either
    bit as non-canonical, and Linux clears `PSR_BTYPE_MASK` before publishing
    `SPSR_EL1` in `struct pt_regs`. The focused fake-KVM negative and native
    65-test backend suite are green. A fixed-root native kernel publication
    rejected the planted live-counter and LL/SC probes and found neither class
    in the real kernel or vDSO. The resulting two-session HVF/KVM game run has
    byte-identical full RAM, portable vCPU state, state hashes, campaign
    artifacts, and normalized endpoint traces.

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

- **PASS — KVM/arm64 interrupt delivery:** the backend creates a stock in-kernel
  GICv3 with 96 implemented IRQs, injects Harmony's level-triggered PPI27 through
  `KVM_IRQ_LINE`, and reports guest acceptance only after the vGIC pending-to-
  active transition. The host-backed architectural virtual timer is moved to
  PPI20 before first entry, leaving PPI27 exclusively owned by the deterministic
  exit-count clockevent. Linux's live boot reports one redistributor, 64 SPIs,
  and reaches the real PPI27 delivery boundary.
- **PASS — byte-identical M1 image inputs:** before the final corpus, msr1
  independently reported the exact M1 hashes:

  ```text
  Image:              41cea2eb60e4155b31ac70300ff9c15205b1533a7b7ab9fb7642bdb17628a3c7
  initramfs.cpio.gz:  6194ec4be99b08e68a61f9020fcedd7aae515b00fa63d38a44b9070a23fea053
  ```
- **PASS — M1 boot oracle verbatim:** ten final-head, same-seed boots pinned to
  CPU 10 each reached `HARMONY_AA5_READY`. Each run independently passed the
  production placement checker and the one-exit-late comparator/placement
  negatives. All ten complete normalized text logs compared byte-for-byte and
  had the same file digest:

  ```text
  runs 01..10, each:
    events=15378 raw=15378 schedules=2 deliveries=1 checkpoints=61
    placement=ok late_comparator_event=12621 late_placement_event=12621
    log_digest=750480b533dba5233670a5f11e0fc33cf3651c0210f354c8b8dfc5bf696aaf80
    KVM_ARM64_BOOT_READY event=15377
    state_hash=14618dd2a344da50c9b627496fa3252cc3cb7b091e9ca923e50d065853fadf09
  normalized-log file sha256:
    e2e349bec7c857c6194474a9d989839dca820f3802e42c283ed000ebbe8a77a6
  M4_TEN_RUN_ORACLE_OK normalized_logs=10 watchdogs=0
  ```
- **PASS — vGIC/vCPU save and restore positive:** at the live `/init` boundary,
  the harness captures the exact VM-state and RAM bytes, restores them into the
  running KVM composition, and requires immediate canonical `state_hash`
  equality. The vCPU record includes core, EL1 sysregs, SIMD/FP, debug state,
  virtual timer, MP state, and the canonical in-kernel GIC record. The GIC
  migration record includes distributor/redistributor group, enable, pending,
  active, line-level, priority, PMR, and Group-1 CPU-interface state.

  ```text
  KVM_VGIC_ROUNDTRIP
    state_hash=14618dd2a344da50c9b627496fa3252cc3cb7b091e9ca923e50d065853fadf09
    architectural=ok planted_field=priority planted_index=27
  ```
- **PASS — planted save/restore negative and genuinely independent comparator:**
  the live harness flips priority byte 27 in the typed post-restore
  architectural record. `compare_gic_architecture` rejects and localizes that
  mutation without reading the snapshot encoding or state hash. Its committed
  test repeats the mutation against the userspace model. The backend's fake-KVM
  save/restore test separately round-trips the register and vGIC attribute maps,
  so codec/hash agreement cannot make the live comparator pass vacuously.
- **PASS — stable KVM timer migration ABI:** the first live round-trip exposed a
  virtual-timer-only mismatch. Component digests localized it before the
  criterion was claimed. Linux's stable arm64 KVM UAPI has historically swapped
  the one-register IDs for `CNTV_CVAL_EL0` and `CNTVCT_EL0`; using the required
  UAPI ID removed the live-counter residue. A committed numeric-ID regression
  test pins the swapped ABI value, and a two-run full-log precheck passed before
  the ten-run corpus began.
- **PASS — honest backend capability publication:** the stock backend reports
  `name=kvm-arm64-vgicv3` and `in_kernel_gic=true`, while keeping
  `deterministic_rng=false`, `deterministic_cntvct=false`,
  `enforces_cntv_cval=false`, and `run_until=Unsupported`. The deterministic
  entropy closure comes from the VMM's seeded stream and the optional firmware
  service bitmaps being disabled, not a false backend capability.

### M4 command evidence

Focused portable gates on the committed implementation:

```text
cargo test -p gicv3 --all-features
16 unit + 3 property tests passed

cargo test -p vmm-backend arm64_kvm --all-features
14 passed

cargo test -p vmm-core --test arm64_skeleton --all-features
21 passed

cargo clippy -p vmm-core --all-features --all-targets -- -D warnings
exit status 0 (pre-existing clippy.toml invalid-path notices only)

cargo fmt --all -- --check
exit status 0
```

The push gate on commit `71b5b62e` ran the full local fast suite:

```text
cargo clippy --all-features --all-targets -- -D warnings: exit status 0
cargo nextest run --all-features: 1280 passed, 25 skipped
cargo fmt --all -- --check: exit status 0
```

Native Linux/aarch64 checkpoints on msr1 compiled the live binary and ran the
following final gates:

```text
cargo test --release --locked -p vmm-backend arm64_kvm --all-features
14 passed

cargo clippy --locked -p gicv3 -p vm-state -p vmm-backend -p vmm-core \
  --all-features --all-targets -- -D warnings
exit status 0 (pre-existing clippy.toml invalid-path notices only)

cargo fmt --all -- --check
exit status 0
```

The changed unsafe crate passed its pinned macOS Miri gate:

```text
MIRIFLAGS=-Zmiri-permissive-provenance \
  cargo +nightly-2026-06-16 miri test -p vmm-backend --all-features
60 library tests, 20 run-loop tests, 1 vCPU-state test, and all remaining
integration targets passed; no UB reported
```

The first native strict-Clippy run found two redundant `u32::from` conversions
in the Linux/aarch64 IRQ-line encoder. They were removed before the final green
run and before M4 was sealed. The final corpus is retained at
`/tmp/harmony-m4-pair-71b5b62e` on that host.

**M4 overall: PASS.** The stock in-kernel delivery decision, byte-attested M1
image, ten-run full-log oracle, independent placement checker, production
planted negatives, exact save/restore positive, typed independent GIC comparator,
and honest capabilities are green. No M5 work began before this evidence was
recorded.

## M5 — bidirectional HVF↔KVM determinism and snapshot portability

- **PASS — byte-attested boot fixture:** the freshly published canonical-PSTATE
  kernel is 2,945,032 bytes and SHA-256
  `47c6eac9d81f69d218decf500fcd4bb77a06917d4ed1bcdceb1900ace315bc96`
  on both hosts. The immutable 1,313-byte initramfs is SHA-256
  `6194ec4be99b08e68a61f9020fcedd7aae515b00fa63d38a44b9070a23fea053`
  on both hosts. The native publication run applied every owned kernel patch,
  rejected planted live-counter and LL/SC probes, and found neither class in
  the real `vmlinux` or vDSO.
- **PASS — full same-seed boot normalized log and checkpoint sequence:** the
  signed HVF run and CPU-0-pinned KVM run each produced 38,453 portable events,
  283 schedules, 136 deliveries, and 151 checkpoints; both placement checkers
  were green. Their normalized-log digest was
  `e2e7852e870648fe59615e2a06ddfdcf56fc6e5e2622fc1e2312cc5250989829`
  and their final canonical state hash was
  `1dc0c1da1b381e992f93d909741d41592bc6dd2ee51fd45f29fd040a1c178b17`.
  The independently transferred text logs were byte-identical: 38,740 lines,
  5,954,217 bytes, SHA-256
  `4b4e7a2758d4eead327bf999ef8324e26fe672429fac2c4b5ae7df396c9a27db`.
- **PASS — byte-attested NES campaign artifacts and endpoint state:** both hosts
  consumed the same 3,209,224-byte game kernel
  (`8cd386f8fcc3a6010f47b39c0a6aae50dbacdde2d1e36529a6dc926c618ea116`),
  637,541-byte initramfs
  (`887abba880be0af63807c3219d4f4300aa2736b4a1fdbfd28e7a4b30ae4bd239`),
  and 40,976-byte ROM
  (`0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`).
  The optimized HVF run and CPU-0-pinned KVM run completed both control
  sessions with empty server and searcher stderr. Session 0 had normalized
  digest `5cbb8b1342529de3aee5a602183d0be4265b68dfec2e070313dad0b9d55f25c7`
  and state hash
  `741e95a2ba98d340f4d022465bffbdc618a3591a092870c8d5d84c37bf9394d8`;
  session 1 had normalized digest
  `828a880243aab8bb4bc3e70abe6d5d15d46397de11f3b3ef007ada0433ef77af`
  and state hash
  `de72c90958b0213b8f435a48de8bfcc679e58f093f7c206e042cdd1614feed58`.
  Every state-component digest matched. Direct `cmp` over each 128-MiB RAM
  image was byte-exact (session SHA-256
  `c27288a2f30610eae1357d9c91f77d61109ff1946e7ea0c1aa210b912143fba4`
  and `0a02df734b40d3f57d69530005330dd3349e2028b9a8aae11bd5a675d41462b8`),
  and the substrate-neutral portion of the exact vCPU dump matched in both
  sessions. The four replayable campaign artifacts were byte-identical:
  `archive-live.json` SHA-256
  `384d30293fdbc766235e4b9a77ac3241ce2679d144cccb79f99a82cc2e4df6b2`,
  `campaign-report.json`
  `da32e0eb13bdf78ce28fcf3898bf437f8c631b5d0029300733aeb374a95e6eff`,
  `stream.jsonl`
  `5e00fcb0c34919b82ef2ee37275f01d55683f1bbd8d7146e44a56c79f6cba6f4`,
  and `snapshots-live.bin`
  `ae8d699c7bdb86180e3232452ee4a83441979be412deae8bffb359acb2f1b783`.
- **PASS — full NES campaign normalized log and checkpoint sequence:** the
  control server now retains an ordered sequence of every restore-delimited
  normalized log and immutable deadline schedule. It records whether each
  segment began at initial boot, a branch, a replay, or a recoverable-restore
  boot; this keeps real V-time rewinds and segment-local event/schedule indices
  structurally valid instead of flattening them into a false single run. Both
  production composition roots fail closed if the existing independent
  placement checker rejects any segment, print total segments/events/schedules/
  checkpoints and the full-session digest, and write a host-neutral complete
  log under `HARMONY_CONTROL_DUMP_DIR`. Two independently driven portable ARM
  control servers produced and compared all three segments across two real
  replay replacements; the comparator's planted negative localized a one-V-ns
  change in the middle segment to segment 1, event 0, `VnsAfter`. Focused
  implementation evidence is green (`cargo test -p vmm-core --all-features
  --lib`: 521 passed, two Miri-only ignores; strict all-target Clippy: exit 0).
  Revision `959dadac` then reran the byte-attested one-job/two-session NES
  campaign on signed HVF and CPU-0-pinned KVM. Session 0 matched at three
  segments, 50,931 portable events, 393 schedules, 198 checkpoints, and session
  digest
  `0c4936c414293a0fde17dfc9bfb0e31560bc9d044081db695b2c2833af5f7478`;
  session 1 matched at four segments, 50,934 portable events, 393 schedules,
  198 checkpoints, and digest
  `431057570943436c2c62a98a61b22764089eeef9e5578004793913b4cf54ad21`.
  Every segment passed its independent placement check on both hosts. Direct
  `cmp` accepted both transferred complete logs: session 0 was 51,330 lines /
  8,030,145 bytes / SHA-256
  `a801bfb068200d7f13e7da05197f8868c71469b0f77b6fcc3dec026945279d45`;
  session 1 was 51,334 lines / 8,030,626 bytes / SHA-256
  `ffdd9dbd32a42b8303552a3672374d87f8d2796b62b5f43a2ee96e93e2d16eda`.
  The newly produced archive, report, stream, and snapshot-checkpoint artifact
  hashes also matched across hosts (`384d3029…6b2`, `b3032b4a…c46`,
  `584e0d3a…8b2`, and `ae8d699c…b783`, respectively). Both campaigns and all
  four control sessions exited cleanly. This is the first evidence that covers
  the entire executed normalized/checkpoint sequence rather than inferring it
  from the final VMM suffix or endpoint state.
- **PASS — strict host-neutral midpoint artifact and immediate restore hash:**
  `ControlServer` now exports/imports a total, versioned `HMSNAP01` stream that
  contains the materialized 128-MiB RAM image, exact canonical ARM VM state,
  SDK stream and remaining payload suffix, Net decisions, active fault policy,
  seal cut, and source whole-state hash, followed by a SHA-256 over every prior
  byte. Import verifies the digest, bounded section lengths, nested codecs,
  configured RAM size, and destination ARM state decoder before minting a
  handle. The artifact taken at frame 365 / V-time 2,158,003,000 / SDK event 9
  was 134,243,185 bytes and SHA-256
  `7639ec2f4512a01613cd60881a0f4dd6fda0dc1ce4a5bd86a864cdaaca3c7c57`
  in both independently produced host directions. Its source cut carried three
  normalized events, zero schedules, was untainted, and carried immediate
  `state_hash`
  `203d76ebf4d53eb26361b5d7473d134aac78d1dfe09f192406ef7ae387175a9e`.
  Both KVM import of the HVF artifact and HVF import of the KVM artifact minted
  handle 1 and reported that exact hash before the first continued action.
  The committed positive restores a complete mid-lineage future; planted RAM,
  VM-state, SDK, Net, and policy corruptions each fail the enclosing digest,
  every truncation and hostile `u64` length is total, and a planted RAM byte is
  rejected before any handle is minted.
- **PASS — both-direction uninterrupted continuation comparison:** source and
  destination used seed `1592642082` and the seed-derived generic action
  vocabulary `(buttons, hold) = (65,4), (193,7), (65,7)`; no route, coordinate,
  or obstacle knowledge enters the driver. Both uninterrupted sources reached
  frame boundaries 365, 372, and 379 with whole-state hashes, in order:

  ```text
  203d76ebf4d53eb26361b5d7473d134aac78d1dfe09f192406ef7ae387175a9e
  bf4d964e23e520feec19957a1532d3a02af8b884397afbf65f558b38e6968d14
  0e23f616ca77e285ecc854a22797d0105bcd669dd16bec929301ee182c281e64
  ```

  Each opposite-host restore reproduced the same three hashes and frames. Each
  source full session contained three restore-delimited segments, 50,940
  portable events, 393 schedules, 198 checkpoints, session digest
  `67d778f64fbe38c0e7e3e5b310b5d16e9ef61564f0169977b2a803f6808bfd6d`,
  and passed independent placement. The source text logs were byte-identical
  across hosts (51,339 lines, 8,031,450 bytes, SHA-256
  `6d2cdf918e3a52975e00c92a76d308a8e78c392e2f855d82b59fdf4cbf189976`).
  Each destination trace had the imported replay segment followed by the exact
  six-event source suffix, zero schedules, and session digest
  `5bbeec2319a5ebafc431235b5d5abda9b2d7c2dcb33b816b454d705e27dafa90`;
  the destination text logs were likewise byte-identical (SHA-256
  `01b775d12cfdb0857a1af6693d9258e5869d774fd4db3e036e9c07a392a1801d`).
  `compare-m5-portability.py` independently verifies each trace's committed
  body digest, rebases the source event/schedule cut, compares every remaining
  event class, payload digest, post-advance V-time, interrupt placement,
  embedded checkpoint, schedule record, and the three out-of-band whole-state
  boundaries. It printed `M5_CONTINUATION_OK events=6 schedules=0
  checkpoints=0 boundaries=3` in both directions. The zero embedded checkpoints
  are non-vacuous here because all three action boundaries carry the independent
  whole-state hashes above; the complete campaign checkpoint sequence is the
  preceding full-session oracle.
- **PASS — planted cross-host increment mismatch:** the production normalized
  comparator's committed one-V-ns perturbation reports event 1 / `VnsAfter`;
  `cargo test -p vmm-core --test prescriptive_vtime
  comparator_rejects_one_vns_increment_at_the_exact_event -- --exact` passed
  before the real boot equality was accepted. The new continuation comparator
  additionally rebases a nonzero cut and localizes a planted increment to
  relative event 1 / `VnsAfter`; its live evidence parser localized an injected
  increment to relative event 2 in each direction and printed
  `M5_CONTINUATION_NEGATIVE_OK`. A separately planted boundary-hash byte was
  localized to boundary 1, proving the external state sequence is also
  load-bearing.
- **PASS — independent architectural-state comparator:** the comparator reads
  live ARM state directly through `Backend::save`, removes any backend-owned
  GIC record from the vCPU, and normalizes it to the same typed GICv3
  architectural form as the userspace model. It does not call the snapshot
  codec, `state_blob`, component digests, or `state_hash`. A fixed
  `consonance.arm64-architecture.v1` writer emits every compared core register,
  sysreg, SIMD/FP byte, debug register/control, quarantined virtual-timer field,
  pending IRQ/FIQ level, MP state, and normalized GIC register/bitmap/priority
  field explicitly. The final source and opposite-host-restored captures were
  byte-identical in both directions: 1,349 lines, 30,316 bytes, SHA-256
  `4c95264e85ac4b11dc08901e689faa3136e09a7f6596b3efefbfad4bb5b7b6dc`.
  The typed comparator's planted core-register negative reports `core.x[7]`;
  the independent GIC comparator reports planted priority corruption at INTID
  27 and a planted distributor-control corruption by name. An embedded backend
  GIC is rejected as non-canonical rather than silently compared twice.

### M5 portability command evidence

```text
cargo test -p vmm-core --all-features --lib portable -- --nocapture
10 passed; 0 failed

cargo test -p vmm-core --all-features --lib \
  vendor::arm64::comparator_tests -- --nocapture
3 passed; 0 failed

compare-m5-portability.py ... --source-event-cut 3 --source-schedule-cut 0
M5_CONTINUATION_OK events=6 schedules=0 checkpoints=0 boundaries=3

compare-m5-portability.py ... --plant-vns-relative-event 2
M5_CONTINUATION_NEGATIVE_OK location='event 2' field=VnsAfter

compare-m5-portability.py ... --plant-boundary-hash 1
M5_CONTINUATION_NEGATIVE_OK location='boundary 1' field=StateHash

cmp <source>.arch <restored>.arch
exit status 0 in HVF→KVM and KVM→HVF directions

pre-push fast gate after the portable comparator:
1304 tests run: 1304 passed, 25 skipped
pre-push fast gate after the independent architecture record:
1305 tests run: 1305 passed, 25 skipped
```

The game kernel and initramfs were re-attested around the reverse run on both
hosts with the same SHA-256 values recorded above; the transferred
artifact and source report reproduced their pre-transfer hashes exactly. Every
KVM runtime in this evidence was pinned with `taskset -c 0`.

**M5 overall: PASS.** Byte identity, full boot and campaign logs/checkpoints,
archive equality, bidirectional immediate restore and uninterrupted
continuations, production and live planted negatives, independent direct ARM
state comparison, and in-kernel/userspace GIC architectural normalization are
all green. M6 did not begin before this evidence was recorded.

## M6 — instrumented concurrency-discovery measurement

- **FAIL — SDK threshold protocol:** not started.
- **FAIL — deliberately racy Go/Rust suite with known schedules:** not started.
- **FAIL — deterministic seeded reproduction:** not started.
- **FAIL — held-out schedule discovery within predeclared budgets:** not started.
- **FAIL — wrong-schedule negative for every entry:** not started.
- **FAIL — held-out schedules absent from seeds/fixtures and per-bug report:** not
  started.

## Repository-wide final gates

- **PASS at M4 — `cargo build --all-features`:** exit status 0.
- **PASS at M4 — `cargo nextest run --all-features`:** 1280 passed, 25 skipped.
  The first sandboxed invocation was correctly treated as invalid evidence after
  localhost listener creation returned `Operation not permitted`; the identical
  suite passed with its normal loopback permission.
- **PASS at M4 — `cargo clippy --all-features --all-targets -- -D warnings`:**
  exit status 0 (the pre-existing invalid-path notices from `clippy.toml` remain
  non-fatal).
- **PASS at M4 — `cargo fmt --all -- --check`:** exit status 0.
- **PASS at M4 — `cargo deny check`:** advisories, bans, licenses, and sources all
  green.
- **PASS at M4 — changed unsafe crate Miri:** the pinned vmm-backend suite passed
  60 library tests, 20 run-loop tests, the vCPU round-trip, and every remaining
  integration target under permissive provenance; no UB was reported.
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
