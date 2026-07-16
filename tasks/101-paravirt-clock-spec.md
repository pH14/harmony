# Task 101 — Paravirt work-derived clock: design spec (ARM correctness + x86 perf)

Bead: `hm-8h8` (P1). Doc-only task: author `docs/PARAVIRT-CLOCK.md`, the design spec for
routing guest time reads through a **work-derived paravirtual clock page** instead of
trapping counter reads. Spec only — no implementation; a separate bead implements after
ratification.

## Why this exists (both must be argued in the doc)

1. **ARM correctness (the forcing function).** No reachable ARM server chip has FEAT_ECV
   (Altra/N1 is v8.2; Graviton 3 = Neoverse V1 and Graviton 4/Grace = V2 both lack it), so
   guest `CNTVCT_EL0` reads cannot be trapped there. We own the guest kernel: its time
   reads must come from a page the vmm derives from **work** (the deterministic V-time),
   and raw counter access must be closed at the CPU-contract level (trap where possible,
   deny/undef + build-exclusion elsewhere — cross-reference the ARM contract stage in
   `tasks/100-arm-vendor-spike-doc.md`).
2. **x86 performance (the free win).** RDTSC exits dominate the hot path on some
   workloads; the nested-x86 N-4 memo recommended sizing exactly this ("a work-derived
   kvmclock-shaped page would remove RDTSC exits from the hot path"). On x86 the RDTSC
   trap REMAINS as the enforcement backstop; the page is an optimization the guest kernel
   opts into.

## Required content

- **Page layout**: versioned, seqlock-style (kvmclock precedent) — fields, widths,
  update ordering, torn-read prevention on a single vCPU. Name the layout's ABI version
  and its place in the state hash (the page is guest-visible state → hashed; say exactly
  how snapshot/restore handles it).
- **Update discipline**: when the vmm refreshes the page (at V-time advance points:
  run_until returns, deadline landings, idle warps — enumerate them against the existing
  VClock/planner seams in `consonance/vtime`), and the determinism argument: every field
  derives from `(work, VClock config)` — never host wall time, never host TSC.
- **Guest-kernel integration sketch**: which kernel clocksource hooks (kvmclock-shaped
  pv_clock on x86; a generic-timer-replacement clocksource on arm64), what the pinned
  kernel config needs, and how the build proves no raw-counter fallback path survives
  (reachability gate — the LL/SC-scan discipline transposed to counter instructions).
- **Per-arch closure story**: x86 = page + retained RDTSC/RDTSCP trap backstop (defense
  in depth); ARM = page + contract-level denial of raw `CNTVCT`/`CNTPCT` access (ECV trap
  where silicon has it — record it as a probed fast-path, never a dependency).
- **Migration path**: how the existing x86 `VClock::tsc()` arithmetic maps onto page
  fields (the vtime crate is arch-blind; the leak is naming only, per ARCH-BOUNDARY —
  this spec should propose the `tsc_hz`→guest-clock rename ride-along).
- **Validation plan**: determinism gates that must stay bit-identical when the page is
  introduced (same-seed twice with page on; page-on vs page-off cross-check on x86 where
  the trap path still exists as the oracle); what N-4-style perf deltas to measure.
- **Kill conditions**: what result would invalidate the design (e.g., an unclosable
  guest-visible ordering between page reads and injected events).

## Read first

`consonance/vtime` (`clock.rs`, `planner.rs`), `docs/ARCH-BOUNDARY.md` (vtime is
arch-blind; C-list rename), `docs/ARM-PORT.md` (ECV facts), `tasks/100-arm-vendor-spike-doc.md`
(the consumer), the kvmclock ABI as prior art (cite it; we are not wire-compatible with it
— our slope is work-derived, not wall-time-derived; state this distinction prominently).
Use "vendor" terminology throughout (never "personality").

## Gates (doc task)

- Doc is internally consistent with the vtime crate's actual seams (cite files/lines) and
  ARM-PORT's hardware facts; open a PR on `task/paravirt-clock-spec`; foreman review.
- Close `hm-8h8` on merge (foreman-owned).
