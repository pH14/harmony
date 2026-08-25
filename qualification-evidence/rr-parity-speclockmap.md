# rr parity: the speculative lock-map probe

Issue #180 requires this program's AMD checks to match rr's. This file records the
citation behind the suite's speculative-lock-map probe so the parity is traceable
rather than asserted.

## What rr does

`rr/src/PerfCounters_x86.h`, function `check_for_zen_speclockmap()`, called from
`check_for_arch_bugs()` on AMD Zen parts:

- arms one raw counter: `init_perf_event_attr(&attr, PERF_TYPE_RAW, 0x510825)`.
  rr's own comment gives the encoding: `0x25 == RETIRED_LOCK_INSTRUCTIONS`,
  `+ 0x08 == SPECLOCKMAPCOMMIT`. The `0x51` in bits 16-23 is the event-select
  control field rr carries inside the raw config.
- reads the counter, executes `asm volatile("lock addl $1, %0": "+m" (atomic));`,
  and reads it again.
- concludes the optimization is disabled when the counter is **unchanged**. rr's
  comment states it directly: when the optimization is disabled, the counter for
  retired lock instructions of type SpecLockMapCommit stays at zero.
- a counter that **moved** is a fatal error (`CLEAN_FATAL`), pointing at
  <https://github.com/rr-debugger/rr/wiki/Zen>, unless `force_things` is set. The
  background is rr issue 2034.

## What this suite does

`consonance/cpu-qualification/src/chips.rs`, the `AMD_ZEN` entry:

    lock_probe_event: Some(TableValue::Recorded {
        value: 0x0051_0825,
        source: "rr src/PerfCounters_x86.h check_for_zen_speclockmap",
    })

Same config, `0x510825`, bit for bit. `consonance/cpu-qualification/src/stage0_sys.rs`
runs the `payload::LOCKED` body under that counter and emits two rows:

| row | expectation |
|---|---|
| `spec-lock-map-commits` | zero |
| `spec-lock-map-probe-ran` | nonzero |

The first is rr's condition and rr's direction. The second is a control this suite
adds and rr does not need: the work clock counts the same run, so a reading of zero
that means "nothing was counted at all" — a counter that failed to arm, a payload
that did not execute — cannot pass for a zero that means "no speculative lock map".

## What the box read

Both rows confirmed on every stage-0 run, with `LS_CFG` (MSR 0xC0011020) bit 54 set
on all 32 threads. See `box/stage0-run1/`, `box/stage0-run2/`, and
`box/check-sealed-pack.txt`.

## The correction this replaced

The suite originally had no event for the probe at all and required the counter to
**move**, which is rr's failure condition read as its success condition. Commit
`49e86362`.
