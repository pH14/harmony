# The paravirt work-derived clock — design spec

Status: **design spec (2026-07-12); ratified-to-build x86-first 2026-07-13** (the pre-build
ruling, `docs/ARCH-BOUNDARY.md` §Pre-build ruling — implementation bead `hm-rk5`, sequenced
behind the `hm-b5n` seam keystone; ABI details freeze through that implementation PR's review;
the ARM closure story is validated on silicon at stage AA-5). Spec bead `hm-8h8`. This
document rules the layout, update discipline, guest-kernel integration, per-vendor closure
story, migration path, validation plan, and kill conditions for routing guest time reads
through a **work-derived paravirtual clock page** instead of trapping counter reads.

The design exists for two independent reasons, and the doc argues both because they load-bear
differently — one is a *correctness forcing function*, the other a *free optimization*:

1. **ARM correctness (the forcing function).** No reachable ARM server chip has FEAT_ECV, so
   guest counter reads there **cannot be trapped**. The page is the only way to give an ARM
   guest deterministic time. This is not optional.
2. **x86 performance (the free win).** RDTSC exits dominate the hot path on some workloads;
   the page removes them. On x86 the design is an *optimization the guest opts into*, with the
   existing RDTSC trap retained underneath as the enforcement backstop and the oracle.

The two share one mechanism — a materialized, work-derived time page — so we spec it once and
close it per vendor. "Vendor" throughout (never "personality"), per `docs/ARCH-BOUNDARY.md` §B
and `tasks/100-arm-vendor-spike-doc.md`.

---

## 0. What "work-derived" means, and how it differs from kvmclock

The layout borrows kvmclock's **seqlock-versioned page** shape (prior art:
`MSR_KVM_SYSTEM_TIME_NEW = 0x4b564d01` and `struct pvclock_vcpu_time_info` in the Linux
kernel — `arch/x86/include/asm/pvclock-abi.h`; guest reader in
`arch/x86/kernel/pvclock.c:pvclock_clocksource_read`). **We are not wire-compatible with it**,
and the divergence is the whole point, so state it first:

- **kvmclock's slope is wall-time-derived.** The host measures the physical TSC frequency
  against a real wall clock and hands the guest `(system_time, tsc_timestamp, mul, shift)`; the
  guest then computes `time = system_time + ((rdtsc() − tsc_timestamp) · mul) >> shift`. **The
  guest still reads a live hardware counter** (`rdtsc`) and interpolates from it. Time tracks
  the host's real clock.
- **Our slope is work-derived and our value is materialized.** Every field is a pure function
  of `(work, VClock config)` where `work` = retired counted branches (`consonance/vtime/src/
  lib.rs:6`), never host wall time and never host TSC. Crucially, **the guest reads a finished
  number** — the page carries the already-computed V-time and virtual counter as of the last
  refresh — and performs **no arithmetic against any live counter**. There is no
  `rdtsc()`/`CNTVCT` term in the guest read path at all.

That single change — hand the guest a materialized value instead of a base-plus-live-delta — is
what makes the design work on a chip whose counter we cannot trap (ARM without ECV) and what
lets us delete the counter-read exit on the chip where we can (x86). It also makes
snapshot/restore of the page trivial (§4): absolute materialized values do not reference a
counter origin, so they survive a restore that resets the hardware counter to zero.

The cost of materialization is **resolution**: between two vmm refreshes the guest sees a
frozen clock. §2 shows why that is deterministic and how the staleness bound keeps a
busy-wait-on-time guest live.

---

## 1. Page layout (ABI `HARMONY_PVCLOCK_ABI = 1`)

One 4 KiB page of guest RAM at a **guest-registered GPA**: the guest publishes the address
**once** via the §3.1 transport, the vmm validates it (page-aligned, wholly inside guest RAM,
clear of the transport's frame pages) and pins it for the machine's life — **re-registration
is a guest fault, rejected**; the stamping target never moves. *(RULED at the task-110 review
(foreman, 2026-07-14, flagged for Paul's veto, same window as the §2 stamping ruling): this
section originally said "a fixed, contract-reserved GPA", contradicting §3.1's
publish-and-validate transport; ABI v1 is the guest-registered one-shot GPA — the kvmclock
precedent of an address-carrying registration, and what makes the guest's page placement a
deterministic function of its own build rather than a contract row.)* Single-vCPU (this
project is single-vCPU; the layout reserves a vCPU-index field for a future fan-out but pins
it to 0). All fields little-endian, matching the codebase's wire discipline
(`consonance/vm-state/src/types.rs:11`). Seqlock-versioned exactly as kvmclock, so a
single-vCPU guest reader never sees a torn update:

| Offset | Width | Field | Meaning |
|---|---|---|---|
| 0x00 | u32 | `abi_version` | Layout ABI = 1. Read once at clocksource registration; a mismatch is a guest-side hard fault, never a silent reinterpret. |
| 0x04 | u32 | `seq` | Seqlock counter. **Odd ⇒ update in progress** (retry); even ⇒ stable. The torn-read guard. |
| 0x08 | u64 | `vns` | Materialized V-time in nanoseconds = `VClock::vns(work)` at the refresh's work count (`consonance/vtime/src/clock.rs:74`). The generic-timer/monotonic clocksource value. |
| 0x10 | u64 | `guest_clock` | Materialized virtual counter — the **guest-visible** clock: `VClock::guest_ticks(work)` **plus the vendor clock-offset register** (x86: `IA32_TSC_ADJUST`, wrapping mod 2⁶⁴), exactly what the retained RDTSC trap completes with, so the §6 G2 oracle equality is definitional (offset `0` for every audited payload). *(Offset term recorded at the task-110 review under the stamping ruling.)* The vendor-counter analogue the guest's counter-shaped clocksource reads. |
| 0x18 | u64 | `guest_clock_hz` | Counter frequency in Hz (`VClockConfig::tsc_hz`, renamed §5). Constant for the machine's life; lets the guest scale `guest_clock` to ns if it wants a counter-native read. |
| 0x20 | u32 | `flags` | Bit 0 `MATERIALIZED` (always 1 for this ABI — signals "value is finished, do not interpolate against a live counter"). Bit 1 `WORK_DERIVED` (always 1 when a **real stamping path** writes the page — the values derive from the deterministic work counter; a *static placeholder page*, e.g. the ARM vendor spike's pre-integration stand-in, deliberately leaves it clear so a consumer or gate that requires it — AA-5 — **fails closed** against a page nothing is actually deriving. *Ruled at the PR #108 r9 / task-110 coordination, 2026-07-14.*) Remaining bits reserved-zero. |
| 0x24 | u32 | `vcpu_index` | Pinned 0. |
| 0x28 | .. | reserved-zero | To end of page. |

**Update ordering (single-vCPU seqlock, kvmclock precedent).** The vmm writes:

```
seq ← seq | 1          // make odd: "update in progress"
write barrier
vns, guest_clock, ...  // publish the new materialized values
write barrier
seq ← (seq + 1) | 0    // make even: stable, one epoch newer
```

The guest reads:

```
do { v = seq; if (v & 1) continue; rmb();
     read vns / guest_clock; rmb();
} while (seq != v);
```

Single-vCPU means the writer (vmm, guest paused at an exit) and the reader (guest, running)
never race — the vmm only writes while the guest is *not* executing, so the seqlock is belt-and-
braces against a future SMP guest and against a reader that straddles the resume boundary. It is
kept because it is free and because the ABI must not have to change to add a second vCPU.

**No `tsc_timestamp` / `mul` / `shift` fields.** Their absence is deliberate and load-bearing:
those are exactly the fields a guest would use to interpolate from a live counter, and we
forbid that (§0). Their omission is what makes the page snapshot-trivial (§4).

### 1.1 Place in the state hash

The page **is guest RAM** at a fixed GPA, so its bytes are already inside the machine's memory
image and therefore already covered by the memory hash and the dirty-log capture
(`consonance/vmm-core/src/snapshot.rs`); it is **not** a new `vm-state` section. It is
guest-visible state, so it *is* hashed — the requirement is that the hashed bytes be a **pure
function of `(work, VClock config)`**, carrying **zero refresh-history entropy**. Two hazards
and their rulings:

- **`seq` carries refresh count.** If the guest is snapshotted with whatever `seq` happened to
  accumulate, and a same-seed sibling run refreshed a different number of times before the same
  Moment, the two pages hash differently → nondeterminism.

  **Original ruling (superseded): re-stamp the page to canonical form at every seal/snapshot
  quiescent point** — `seq = 0`, values at the exact seal work count `w`, reserved tail zeroed —
  so the sealed page is a total function of `(w, config)`. Its safety argument was that "the
  guest only reads the page while running, never at the seal boundary (a seal is taken at an HLT
  quiescent Moment)".

  **AMENDED RULING (PR #110 cross-model r4, 2026-07-14): the page is sealed VERBATIM; nothing
  canonicalizes a live page except registration.** The original ruling's premise is false —
  since **task 41** a seal is taken at *any* V-time-synchronized intercept, not only at an HLT
  quiescent Moment, so a guest reader **can** be mid-seqlock-read across a seal. Resetting a
  live `seq` to a fixed epoch is then an **ABA**: a reader that sampled `seq = 0`, took an exit
  before its validating re-read, and resumed after a refresh-then-canonicalize would see `seq =
  0` again, accept the values it had already loaded, and miss the refresh — *taking a snapshot
  would change the guest's future*. Canonicalizing only the snapshot **copy** is no better: it
  makes the sealed image differ from live guest RAM, which breaks both the snapshot engine's
  derive path (`snapshot_derive` diffs live RAM against the parent *image*) and same-state ⇒
  same-future (a parent that seals and continues would diverge, by exactly its `seq`, from a
  child restored from its own snapshot).

  What replaces canonicalization is **value-keyed stamping** (§2, ruled at r1): a stamp that
  publishes values the page already carries writes **nothing** and does not move the epoch. So
  `seq` advances only on *distinct-value* publications, whose stream is a pure function of the
  deterministic execution — the epoch is reproducible by construction, and a restored run
  inherits its parent's epoch and continues in lockstep. Two same-seed runs cannot refresh a
  *different number of value-changing times*, which is the only thing the page bytes see.

  The two fragilities the original ruling cited are closed by other rulings in the same PR, so
  the "accident of scheduling" it feared is now a contract: **backend skid** cannot reach the
  values (stamps use the skid-free `last_intercept_work` anchor, r1), and **Δ** is machine
  configuration carried in the sealed device blob and cross-validated on restore (r3) — a Δ
  mismatch is *rejected*, never silently divergent. Canonical form survives as the
  **registration** form (a fresh page, no reader possible, no prior epoch to alias), which is
  what gives the channel a known starting epoch and a zeroed tail regardless of what the guest's
  allocator left behind. The determinism gate (§6, G1) is what proves the whole story.

---

## 2. Update discipline — when the vmm refreshes the page

The vmm re-stamps the page from the current `VClock` at **V-time advance points**, all of which
are exits the run loop already takes, plus one bounded forced refresh. Enumerated against the
existing `consonance/vtime` seams:

1. **`run_until` / any natural exit returns.** Every time `KVM_RUN` exits (hypercall, MMIO,
   deadline, HLT, the forced overflow below), the vmm re-stamps the page with the clock at the
   **skid-free anchor** — the work count of the last deterministic clock-advance boundary
   (`last_intercept_work`: the V-time intercepts, `Deadline` landings, and idle warps), the
   same value the RDTSC-trap oracle returns — and stamps
   `vns = VClock::vns(anchor)`, `guest_clock` = the guest-visible clock at `anchor` (§1). The
   stamp is **value-keyed**: between two clock advances the anchor cannot move, so the refresh
   at a non-intercept exit republishes identical bytes (a no-op), and the published value
   stream advances exactly at the deterministic boundaries. This is the natural,
   zero-added-cost refresh: the exit was going to happen anyway. *(Amended at the task-110
   implementation review — the foreman's 2026-07-14 stamping ruling, flagged for Paul's veto:
   the original text read "the current work count from `CpuBackend::work()`", but a live
   counter read at a non-intercept boundary carries non-deterministic exit-path skid (the
   task-27 O1 evidence), and the page is hashed guest RAM — the literal reading contradicted
   this section's own determinism argument. The anchor formulation is the intent made
   implementable.)*
2. **Deadline landings.** When `TimerQueue::pop_due` (`consonance/vtime/src/queue.rs:106`) fires
   a timer, the run loop is at the exact injection work count reached by
   `InjectionPlanner::stop_at` → `PlanOutcome::ReadyToInject` (`planner.rs:119`). The page is
   re-stamped **before** the interrupt is injected, so a guest that reads the clock in its ISR
   sees a `vns`/`guest_clock` consistent with the interrupt's own V-time. See the kill condition
   in §7 (page/injection ordering) — this refresh-at-injection is what closes it.
3. **Idle warps.** On an HLT idle-skip the run loop applies `IdlePlanner::plan` →
   `VClock::advance_idle(advance_vns)` (`consonance/vtime/src/idle.rs:102`, `clock.rs:131`),
   which moves `vns_base` forward without executing an instruction. The page is re-stamped to
   the warped clock at the landing Moment `D`, so a guest woken by the timer reads the advanced
   time, not the pre-idle time. Because `advance_idle` only ever moves `vns_base` forward, the
   stamped `vns`/`guest_clock` are monotonic across the warp.
4. **Staleness-bound forced refresh (the one added exit).** A purely compute-bound guest that
   spins reading the clock (`while (now() < deadline)`) takes **no** natural exit, so without
   this the page would freeze and the loop would hang — the page's one correctness obligation
   beyond determinism. The vmm therefore arms a PMU overflow at `work_ref + Δ`
   (`CpuBackend::run_until_overflow`, `planner.rs:25`) whenever the next scheduled event is
   farther than Δ counted events away, forcing an exit-and-refresh every Δ work units. Δ is a
   tunable that trades resolution for exit rate; §6 measures it. **This is the perf story
   made precise:** RDTSC-trap x86 pays one exit *per counter read*; the page pays one exit *per
   Δ-work window*, batching an unbounded number of reads into one refresh.

**Determinism argument.** Every stamped field derives from `VClock::vns`/`guest_clock` applied
to a `work` count that is itself a pure function of the deterministic instruction stream
(`consonance/vtime/src/lib.rs:24`), plus `vns_base` moves that come only from idle-skip and
snapshot restore (`lib.rs:30`) — the two V-time-without-work events the crate already
enumerates. No field reads host wall time or host TSC. The *set* of refresh Moments is a pure
function of the seed (natural exits are deterministic; the forced-refresh arming at `work_ref +
Δ` is deterministic; deadline landings are exact via the single-step planner). Two same-seed
runs therefore stamp identical values at identical points in the instruction stream ⇒ the guest
reads bit-identical time ⇒ bit-identical execution. The staleness refresh does **not** perturb
work: it is a forced exit at a work count, and the guest resumes at that exact count — the same
arm-then-observe contract the injection planner relies on.

**What materialization costs, restated as a guarantee, not a bug:** between refreshes the guest
sees a piecewise-constant clock. It is monotonic non-decreasing (`VClock::vns`/`guest_clock` are
monotonic in work, `clock.rs:74`/`85`, and refreshes only publish forward values) and it is
deterministic. A guest may read the same timestamp across many instructions; that is legal for a
coarse clocksource and is exactly what the resolution/Δ knob governs.

---

## 3. Guest-kernel integration sketch

We own the guest kernel, so integration is a pinned clocksource plus a build-time proof that no
raw-counter path survives.

### 3.1 x86 — a kvmclock-shaped `pv_clock` clocksource

- **Hook.** A clocksource whose `.read()` performs the §1.1 seqlock read and returns
  `guest_clock` (counter-native) or `vns` (ns-native), registered at high rating so the kernel
  prefers it. Shaped like `arch/x86/kernel/kvmclock.c` but with the interpolation arithmetic
  deleted — `.read()` is a page load, not `base + rdtsc()·mul>>shift`.
- **Page registration transport.** The guest publishes the page GPA to the vmm via the existing
  hypercall doorbell (`docs/GLOSSARY.md` hypercall-doorbell) or a contract-reserved MSR write
  that the CPU contract handles; the vmm validates the GPA lands in guest RAM and begins
  stamping. (Exact transport is an implementation choice for the follow-on bead; both are
  already-modeled seams. Task 110 chose the doorbell — `hypercall-proto` service id 7 —
  with the host advertising its offer via the `harmony_pvclock` kernel parameter.) Registration
  is a **two-step handshake** (the r8 ruling). The doorbell `OUT` only **records a pending
  registration** — it does not stamp the page or arm the Δ forced-refresh deadline, because the
  `OUT` is a plain PIO exit, not a V-time intercept: the work counter read there carries
  exit-path skid, and the pre-`OUT` anchor may be stale. The guest then **must** execute one
  V-time intercept — an `rdtsc` — immediately after the doorbell; this **handshake intercept** is
  where the host lays down the first (canonical) page stamp and arms the Δ deadline, both off the
  intercept's fresh, skid-free anchor (so the target `anchor + Δ` is current and can never be
  born overdue). The post-doorbell `rdtsc` is therefore **protocol, not courtesy** — a guest
  that omits it is **out of contract**: its page stays at the pre-registration bytes (stale but
  deterministic) and no refresh arms, which is an acceptable degradation for a non-conforming
  guest and requires no host-side arming off a skid-tainted or stale anchor. *(Ruled at the
  task-110 r8 review; supersedes the r5/r6 "fresh work read at the OUT … like RDTSC" wording and
  the r7 "immediate arm off the existing (possibly stale) anchor" wording — the former anchored
  on a skid-tainted PIO read, the latter could arm an overdue deadline whose landing imports a
  live count. The handshake resolves both: nothing arms until a genuine skid-free intercept.)*
- **Pinned kernel config / no-fallback.** `CONFIG_HARMONY_PVCLOCK=y` (the new clocksource),
  and the TSC clocksource must be **unselectable**: mark TSC unstable / drop it from the
  clocksource registry so the kernel can never fall back to raw `rdtsc` for timekeeping. RDTSC
  from userspace still traps (§4) as the backstop — the config change closes the *kernel's
  timekeeping* path, the trap closes the rest.

### 3.2 arm64 — a generic-timer-replacement clocksource

- **Hook.** Replace `drivers/clocksource/arm_arch_timer.c`'s `arch_counter_get_cntvct` reader
  with the page reader. The kernel's `arch_sys_counter` clocksource is **removed from the
  build**, not merely deprioritized — on non-ECV silicon it would read a real, non-work-derived
  `CNTVCT` and instantly break determinism.
- **Pinned config / no-fallback.** The guest kernel is built without the arch generic-timer
  clocksource as a selectable source; the page clocksource is the only timekeeper. Its virtual
  and physical clockevent `set_next_event` paths write an absolute page-derived tick deadline to
  `docs/INTEGRATION.md` §1.3 and never program `CNTV/CNTP_CVAL/TVAL`; the virtual timer remains
  disabled. The host evaluates that deadline only after an exact-work page publication, then
  holds dedicated level-triggered PPI 20 high until the guest ACKs before calling the generic
  event handler. The owned DT places PPI 20 in the architected-timer virtual-interrupt slot and
  requires `nohlt` polling, because a single vCPU stopped in WFI cannot retire the branches that
  advance work time. KVM level-info readback proves each line transition rather than treating a
  successful injection ioctl or the separate pending latch as delivery evidence.
- **Registration transport.** The owned ARM guest publishes its selected 4 KiB page GPA through
  `docs/INTEGRATION.md` §1.3's one-shot MMIO register. The natural MMIO exit only records the
  validated pending GPA; it never imports that exit's skid-tainted PMU count. The first exact
  arm-early + single-step cadence landing canonically stamps the page, and the guest remains in a
  bounded deterministic spin until that stamp appears. Any second registration is a guest fault,
  including a repeat of the same GPA. This is the ARM substitute for x86's post-doorbell RDTSC
  handshake: non-ECV N1 has no counter-read intercept, so exact forced landing is the only lawful
  first anchor. The owned build's `2^28`-iteration spin is paired with a host-enforced
  `Δ <= 100_000_000` retired-branch ceiling, so the first target cannot lawfully outlive the poll.

### 3.3 Reachability gate — no raw-counter path survives (LL/SC-scan discipline transposed)

The "prove no raw counter read survives" obligation is the counter-instruction analogue of
task-100's LL/SC opcode-scan discipline (`tasks/100-arm-vendor-spike-doc.md` §4; `docs/
ARM-PORT.md:60`). The build gate scans the final guest kernel image (and any reachable module)
for raw counter opcodes:

- arm64: `mrs xN, CNTVCT_EL0`, `mrs xN, CNTPCT_EL0`, `mrs xN, CNTVCTSS_EL0`/`CNTPCTSS_EL0`;
- x86: `rdtsc` (`0F 31`), `rdtscp` (`0F 01 F9`).

The scan inherits task-100's enforcement ladder: kernel-config guarantee → static opcode scan →
**W^X + rescan-on-exec** for any page the guest makes executable at runtime (so a JIT/self-modifying
guest cannot introduce a counter read the static scan never saw). A guest that can mint executable
counter-read code the vmm cannot re-scan is out of contract — see §7.

**The bar is per-vendor, because closure is (§4):**

- **x86 — reviewed reachable reads are allowed, trap-backstopped.** The gate is an *allowlist*: a
  raw `rdtsc`/`rdtscp` left in the image is admissible **iff** it is a known, reviewed site
  (recorded `symbol+0xOFFSET`), because the retained RDTSC/RDTSCP trap (§4.1) completes *any*
  reachable read — allowlisted or not — with the same work-derived value the page carries. A
  reachable read is therefore **contract-safe** on x86, never a determinism hole; the allowlist
  exists only to force human review of *new* reads (so nobody adds an unreviewed timekeeping path),
  not because a reviewed read is unsafe. An **unlisted or moved** site fails the build.
- **ARM — the bar is strictly zero reachable reads.** There is no `CNTVCT`/`CNTPCT` trap available
  on the reachable non-ECV silicon (§4.2), so a reachable counter read is an *actual* escape from
  work-derived time — unbackstopped. The transposed gate therefore carries an **empty allowlist by
  necessity**: any hit fails the build. This strict zero-reachable requirement is the ARM no-trap
  closure story, validated at spike stage AA-5 — it is **not** the x86 bar.

---

## 4. Per-vendor closure story

The page is the *fast path*; closure is what makes a non-cooperative or buggy guest unable to
escape work-derived time.

### 4.1 x86 — page + retained RDTSC/RDTSCP trap (defense in depth)

The RDTSC/RDTSCP trap **stays**. On patched KVM the instructions surface as
`KVM_EXIT_DETERMINISM` and the vmm completes them with `VClock::guest_clock(work)` at the exit's
work count (`consonance/vmm-backend/src/kvm.rs:520`, capability `deterministic_tsc`,
`consonance/vmm-backend/src/exit.rs:164`). So:

- The page is an **opt-in optimization**: a cooperative guest kernel reads the page and never
  exits; anything that still executes `rdtsc` (userspace, a driver, a miscompiled path) traps
  and gets the *same* work-derived value. There is no way to read a non-work-derived clock.
- The trap is therefore both **backstop** (closure does not depend on the guest cooperating)
  and **oracle** (§6: the page's stamped value must equal what the trap would have returned).

### 4.2 ARM — page + contract-level denial of raw `CNTVCT`/`CNTPCT`

There is no RDTSC-equivalent trap available on the reachable silicon, so closure is structural,
not interception-based:

- **ECV is a probed fast-path, never a dependency.** No reachable ARM server chip has FEAT_ECV:
  Ampere Altra / Neoverse N1 is Armv8.2; Graviton 3 (Neoverse V1) and Graviton 4 / Grace
  (Neoverse V2) both lack it (`docs/ARM-PORT.md:30`, table + §1). Where silicon *does* have ECV
  (e.g. DGX Spark's Cortex-X925, Armv9.2 — `docs/ARM-PORT.md:33`), the vmm may set
  `CNTHCTL_EL2.EL0VCTEN=0` to trap `CNTVCT_EL0` as an extra guard — but this is **recorded as a
  probed capability used if present, never required**. The design must be fully closed on a chip
  with no ECV bit at all.
- **Closure without a trap = we own the guest + contract denial.** On non-ECV silicon `CNTVCT`
  is architecturally readable at EL0 and cannot be trapped, so closure rests entirely on: (1)
  the §3.3 reachability gate (the guest kernel provably contains no reachable counter read), and
  (2) the ARM CPU contract freezing the `ID_AA64*` ID registers and denying/UNDEF-ing the
  system-register surface that would let a guest reconfigure timer routing
  (`docs/ARM-PORT.md:52`, the `HCR_EL2`/`MDCR_EL2` analogue; `tasks/100-arm-vendor-spike-doc.md`
  §5). Determinism on ARM therefore **depends on owning the guest image** in a way x86 does not
  — which is exactly why §7's reachability kill condition is sharper for ARM.

---

## 5. Migration path — `VClock::tsc()` arithmetic onto page fields, and the rename ride-along

The `vtime` crate is arch-blind; the only x86 leak is naming (`docs/ARCH-BOUNDARY.md:47`: "the
leak is the field *name* only"). This spec proposes the rename ARCH-BOUNDARY §C.3 already
scheduled ("rename `VClockConfig::{tsc_hz, tsc_base}` → guest-clock naming, whenever vtime is
next touched") ride along with the page work, since the page introduces the first non-x86
consumer:

| Today (`consonance/vtime/src/clock.rs`) | Renamed | Page field (§1) |
|---|---|---|
| `VClockConfig::tsc_hz` | `guest_clock_hz` | `guest_clock_hz` @ 0x18 |
| `VClockConfig::tsc_base` | `guest_clock_base` | (folded into `guest_clock`) |
| `VClock::tsc(work)` (`clock.rs:85`) | `VClock::guest_clock(work)` | `guest_clock` @ 0x10 |
| `VClock::vns(work)` (`clock.rs:74`) | *(unchanged — already neutral)* | `vns` @ 0x08 |

The arithmetic is unchanged: `guest_clock(work) = guest_clock_base + floor(vns(work) ·
guest_clock_hz / 10⁹)`, computed in `u128`, saturating (`clock.rs:85`). On x86 `guest_clock` *is*
the virtual TSC (`guest_clock_hz` = virtual TSC Hz); on arm64 it *is* the virtual `CNTVCT`
(`guest_clock_hz` = `CNTFRQ_EL0`) — one Hz-scaled counter mapping, which is exactly why
ARCH-BOUNDARY calls `VClock::tsc()` "structurally a generic Hz-scaled counter mapping unchanged
to `CNTVCT`/`CNTFRQ`" (`docs/ARCH-BOUNDARY.md:46`). The `vm-state` mirror
`VtimeState{tsc_hz, tsc_base}` (`consonance/vm-state/src/types.rs:148`) renames in lockstep;
this is naming-only and does not disturb the `ratio_den == 1`-for-snapshots rule
(`types.rs:152`) or the encoded byte layout's meaning.

**Restore is a no-op for the page beyond re-stamping.** On snapshot restore the hardware counter
restarts at 0 and the restored `VClock` carries the whole effective V-time in `vns_base`
(`clock.rs:135`, `VtimeState::snapshot_vns`, `types.rs:158`). Because the page holds **absolute
materialized values** (not a delta against a counter origin), the captured page bytes are already
correct after restore, and the first post-restore refresh re-stamps them from the restored clock
identically to how a never-snapshotted run would stamp them at the same Moment. No page-specific
restore logic, no `VM_STATE_VERSION` bump for the page itself (`consonance/vm-state/src/
lib.rs:69`) — a layout change *would* bump `HARMONY_PVCLOCK_ABI` and, because it changes guest
RAM bytes, be gated as an ABI break at guest-kernel build time.

---

## 6. Validation plan

Two determinism gates that must stay green, one cross-check that uses the x86 trap as an oracle,
and the N-4 perf deltas.

- **G1 — same-seed determinism, page on (bit-identical).** Run the same seed twice with the page
  enabled; require identical `state_hash` at every sealed Moment. This is the primary gate: it
  proves the refresh schedule + canonicalization (§1.1, §2) leak no entropy. Must hold on both
  vendors.
- **G2 — page-stamp vs oracle (x86 only, where the trap survives).** The page's stamping
  function is the same `VClock::guest_clock`/`vns` the RDTSC trap uses. Gate: at every refresh
  Moment, the value written to the page **equals** the value the RDTSC-trap path would return for
  that work count (`consonance/vmm-backend/src/kvm.rs:520` completion vs the §1 stamp). This is a
  *function-equality* check, not a full-machine-hash equality — a page-on run and a page-off run
  legitimately differ in `state_hash` (the page bytes exist in one and not the other, and clock
  *resolution* differs), so hash-equality across the two configs is **not** the gate; the oracle
  is the stamping function. G2 is the x86 safety net that catches a stamping bug the ARM side has
  no trap to catch.
- **G3 — resolution/liveness.** A busy-wait-on-time guest (§2 point 4) must terminate: assert the
  staleness-bound forced refresh advances the page within Δ work units, on both vendors. Prevents
  the "frozen clock hangs the guest" failure the materialized design introduces.
- **Perf (N-4-style, x86; the free-win measurement).** Per `docs/NESTED-X86.md:282` (N-4): the
  page's sizing rationale is "a work-derived kvmclock-shaped page would remove RDTSC exits from
  the hot path." Measure, page-off vs page-on: RDTSC exit rate per virtual second (target: hot-
  path reads → ~0, replaced by one refresh exit per Δ window), boot-to-userspace wall ratio,
  det-corpus + a postgres campaign smoke throughput, and the resulting exits/virtual-second
  delta. Report as ppm-style ratios like the other N-4 memos.

---

## 7. Kill conditions

What result invalidates the design (and which vendor it kills):

1. **Unclosable page/injection ordering (both vendors, primary).** The guest can observe, via the
   page, a time inconsistent with an injected event's V-time — e.g. a timer interrupt injected at
   work `T` (V-time `vns(T)`) delivered while the page still shows `vns(T−δ)` because it was not
   re-stamped at the injection Moment, so the guest's ISR reads a timestamp *earlier* than the
   interrupt that woke it. §2 point 2 closes this by re-stamping **before** injection. The design
   dies if that re-stamp cannot be made to produce, for every same-seed run, the **identical**
   (page value, interrupt) ordering — i.e. if there is any interleaving in which the page read
   and the injection are observably reorderable non-deterministically. The gate that would expose
   it is G1 (a reordering that is nondeterministic breaks same-seed determinism).
2. **ARM reachability escape (ARM only, sharpest).** The §3.3 scan cannot prove that no raw
   `CNTVCT`/`CNTPCT` read survives on a reachable path — concretely, a guest that mints executable
   counter-read code at runtime which the W^X/rescan-on-exec discipline cannot re-scan (a JIT the
   vmm cannot intercept) — **and** the silicon lacks ECV to trap the read. Then the guest reads a
   real, non-work-derived counter and determinism is dead on that target. On x86 the retained
   RDTSC trap makes this survivable (§4.1); on non-ECV ARM there is no trap, so this is a hard
   kill for any guest we cannot fully own.
3. **Staleness/perf collapse (x86 optimization rationale only).** If the Δ needed for correctness
   (G3 liveness for real busy-wait loops) is so small that the forced-refresh exit rate approaches
   the RDTSC-trap exit rate it replaces, the x86 *performance* justification evaporates (the ARM
   *correctness* justification stands regardless — there the page is not optional). Measured by
   the §6 perf deltas; a page-on RDTSC-exit-rate reduction below a set threshold (e.g. <2×) means
   "not worth it on x86," not "broken."

---

## 8. Relationship to the ARM vendor spike (`tasks/100`)

This spec is the design; `docs/ARM-ALTRA.md` (bead `hm-x8g`, task 100) is the consumer that
**validates** it on real N1 silicon. Task 100 §1 names the paravirt work-derived clock as its
time-virtualization centerpiece and defers the design here; the division is: this doc rules
*what the page is and how it closes*, the spike rules *which stage boots a guest whose only
clocksource is the page, proves G1/G3 on the box, and records the kill-condition-2 reachability
ruling against a real guest image*. Neither doc duplicates the other; cross-reference only.
