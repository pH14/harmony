# The skid tail, and what the margin can and cannot do about it

## It is documented Zen behaviour, not a surprise

rr marks every Zen entry `PMU_SKID_UNBOUNDED` with a nominal 10,000 and the note that
it is exceeded in rare cases. Issue #180 priced that caution into this program's terms
of reference when it ruled the restart:

> **Skid re-measurement under `0xd1`,** with rr's field caution priced in: rr marks Zen
> `PMU_SKID_UNBOUNDED` (nominal 10000, "exceeded in rare cases"). The margin derivation
> must state its overshoot-handling story (the loud skid-exceeded error path), not just
> a measured max.

So a measured maximum is not the deliverable on its own. What follows is the tail as a
rate with its denominator, the distribution it belongs to, and the handling.

## Two distributions, not one

The two are measured on different paths and must not be pooled.

**Host-user sampling scope.** Stage 1 arms every overflow with `exclude_kernel` and
`exclude_hv` set and `exclude_host` clear, counting the calling thread's own user-mode
execution (`consonance/cpu-qualification/src/perf.rs`, `Scope::HostUser`, used at
`stage1_sys.rs` `open_sampling`). Four campaigns, 1,250,000 arms each, 5,000,000 in
total. Maximum skid 8,096; the pack's `skid.observed_max`, and its margin of 16,192, are
twice that.

**Guest-only sampling scope.** The stage-2 landing arms `exclude_host`, so only guest
execution counts and the overflow's NMI has to travel out through a virtual-machine
exit. Nothing in stage 1 measures this path: the suite's only guest-scope counter is a
counting one, for the guest exactness payload.

The guest-mode tail is heavier, and that is the finding. Numbers below.

## The margin is derived from the host distribution, deliberately

The pack keeps `skid.margin = 16192`, twice the host-user maximum, and does not derive
it from the guest-mode tail. Two reasons, both stated in the pack:

1. A margin large enough to cover the observed guest tail would be about 75,000, which
   single-steps 4.6 times more work on every landing — the landing cost is `margin -
   skid` instructions — and would still not be provably sufficient, because rr's own
   characterisation of the tail is unbounded. Buying a large constant cost for an
   unprovable guarantee is the wrong trade.
2. The correct handling of an unbounded tail is loud detection plus a retry, not a
   bigger constant.

## The overshoot-handling story

**Detection is loud and an overshoot is never accepted as a landing.** In
`consonance/vtime/src/planner.rs`, `stop_at` returns `VtimeError::SkidExceeded` when the
overflow stops at or past the target, and again after the single-step walk if the work
count ended past the target. The comment states the invariant: every `ReadyToInject`
reached through the overflow phase has been positioned by the exact single-step phase.
The gate test `consonance/vtime/tests/planner.rs`
`skid_exceeding_margin_is_loud` asserts it and passes; it draws skids beyond the margin
until one lands and requires `SkidExceeded` with `stopped_at > target`.

On silicon the same refusal holds structurally: the harness records
`landed_exact = 0`, `overshoot = 1` and fails the arm. An overshoot cannot be mistaken
for a landing at either layer.

**Recovery is re-arming the same target.** Measured; numbers below.

## What a large skid was: measuring the cause

Three probes, of which one is unavailable on this chip and one is inconclusive.

**`MSR_SMI_COUNT` (0x34) does not exist on AMD.** It is an Intel MSR. On this chip
`rdmsr` refuses it on every core:

    cpu0  rdmsr: CPU 0 cannot read MSR 0x00000034
    cpu2  rdmsr: CPU 2 cannot read MSR 0x00000034      (same for cpu3, cpu9, cpu15)

AMD publishes no architectural per-core system-management-interrupt counter, so the
per-arm delta the direction asks for cannot be taken that way.

**The AMD SMI-received PMC event reads zero.** AMD's PPR for family 19h has an
SMI-received counter at PMCx02B; opened as raw `0x51002b` it counts nothing, while the
work clock on the same command counts normally:

                     0      r51002b
                 47855      r5100d1
           2.002202173 seconds time elapsed

That is either "no system-management interrupts occurred" or "this is not the right
encoding for this event on this part". Nothing available here separates the two — a
deliberate system-management interrupt would settle it, and the only software route is
a write to the ACPI command port, which runs firmware code of unknown effect on a box
carrying a running campaign. The reading is recorded and treated as weak.

**Timing separates the two hypotheses that matter.** The instrumented harness stamps
the time-stamp counter immediately before the guest run that ends at the overflow and
again when it returns, so each arm carries the wall-clock length of exactly that
interval alongside the guest branches retired in it. The two candidate causes of a
large skid predict opposite ratios:

- the guest kept running and the overflow's interrupt was delivered late — branches and
  ticks both large, their ratio the population's normal one;
- the core was stopped, in system-management mode or otherwise — ticks large, branches
  not, ratio far below the population.

The normal ratio on this chip is about 0.7 to 0.94 guest conditional branches per
time-stamp tick. The supplement and the reproduction both record it on every arm.

**Interrupt load does not single out the core the overshoots landed on.** Both
overshoots so far are on core 15. `/proc/interrupts` over the campaign gives core 15
160,934 interrupts against 153,703 to 164,171 on the other seven isolated cores, and
its device-interrupt share is among the lowest; the count is dominated by the
performance-monitoring interrupts the campaign itself arms. With two events the
clustering is not yet distinguishable from chance.
