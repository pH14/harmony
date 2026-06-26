# lapic — implementation notes

Userspace xAPIC register file + V-time timer per `tasks/13-lapic.md`, settled by ruling
**R1** (`docs/R1-DEVICE-MODEL.md`). Pure logic, `#![no_std]`, no `unsafe`, no host deps, no
clock reads — V-time in, register reads/writes in, deadlines + deliverable vectors out. All
standard gates and the five task gates pass on macOS (Apple Silicon, rustc 1.94.1); the Kani
job runs on the Linux CI box.

## Design in one paragraph

`Lapic` stores the register file as plain fields (the same set `LapicState` snapshots verbatim)
plus five timer fields — `initial_count` (`N`, the TMICT register & reload value), `count_at_arm`
(the count remaining at the anchor), `timer_arm_vns` (the anchor), `timer_running` (counting), and
`timer_pending` (count loaded & not consumed). Every time-dependent value is **derived on demand**
from `now_vns`, never ticked: the Current Count register is
`count_at_arm − floor((now − arm)·timer_hz/(divide·1e9))` and the firing deadline is
`arm + ceil(count_at_arm·divide·1e9/timer_hz)`, both in `u128` saturating to `u64`/`u32` (one
`sat_u64`/`sat_u32` helper is the crate-wide overflow rule, mirroring `vtime`). Anchoring on
`(count_at_arm, arm_vns)` rather than a precomputed deadline is what makes the Current Count
round-trip exact for *any* `timer_hz` — at the arming instant `elapsed = 0`, so the count reads
exactly `count_at_arm` (the historical double-floor trap with e.g. `timer_hz = 24_000_000` is
avoided) — **and** lets a mid-count divide change re-anchor from the current remaining instead of
rewriting history. The timer mode comes from the LVT-timer entry; periodic re-arm is closed-form
(reload `initial_count`, `arm += k·period`) so a multi-period `advance_to` jump is drift-free and
never loops. Prioritized delivery models PPR
from TPR and the highest in-service vector; `take_interrupt` is gated on the IRR vector's class
exceeding PPR's class with the APIC software-enabled. There is no `HashMap`/`HashSet`, no float,
and no clock read, so identical inputs give bit-identical reads, deadlines, and `LapicState`.

## Register map (SDM Vol. 3A §11.4, Table 11-1 / §11.x "Advanced Programmable Interrupt
Controller")

Implemented at 16-byte-aligned MMIO offsets within the 4 KiB page:

| Offset | Register | R/W | Notes |
|---|---|---|---|
| 0x020 | ID | RW→RO | Reads `apic_id << 24`; **treated read-only** (single-vCPU frozen ID, contract pins it to 0). |
| 0x030 | Version | RO | `0x0005_0014` — version `0x14`, max-LVT = 5 (six LVTs; CMCI excluded). |
| 0x080 | TPR | RW | Low byte. |
| 0x0A0 | PPR | RO | Derived from TPR + highest ISR. |
| 0x0B0 | EOI | WO | Any write retires the highest ISR bit. |
| 0x0D0 | LDR | RW | Bits 24..=31. |
| 0x0E0 | DFR | RW | Reset `0xFFFF_FFFF`; reserved bits read 1. |
| 0x0F0 | SVR | RW | Bit 8 = software enable; vector + focus + EOI-suppress writable. |
| 0x100–0x170 | ISR | RO | 256 bits, 8 words. |
| 0x180–0x1F0 | TMR | RO | 256 bits, 8 words. |
| 0x200–0x270 | IRR | RO | 256 bits, 8 words. |
| 0x280 | ESR | RW | Write clears (see "ESR" below). |
| 0x300/0x310 | ICR low/high | RW | Self-IPI on fixed-mode self/all-incl-self shorthand. |
| 0x320–0x370 | LVT × 6 | RW | Timer (vector+mask+mode), Thermal/PerfMon (+delivery-mode), LINT0/LINT1 (+polarity+trigger), Error (**vector+mask only — no delivery-mode**). |
| 0x380 | Initial Count | RW | Write (re)arms the timer. |
| 0x390 | Current Count | RO | Derived from `now_vns`. |
| 0x3E0 | Divide Config | RW | All 8 encodings legal (bits [3,1,0]). Bit 2 is decode-ignored and **not stored** (`TDCR_WRITE_MASK = 0xB` drops it), so two guests differing only in TDCR bit 2 snapshot/hash identically — see "Divide-config bit 2" below. A mid-count write **reschedules** from the current remaining (re-anchor), never retroactive. |

Reads of unimplemented-but-architectural registers (incl. **LVT CMCI 0x2F0**, which max-LVT = 5
excludes) return 0; writes to read-only or reserved-in-range registers are **silently dropped**
(deny-ignore-write — the CPU/MSR contract's disposition; `vmm-core` does the logging). Only a
misaligned or out-of-range offset is `LapicError::BadOffset`.

## Decisions the integrator should know

- **`LapicState` is the frozen contract** (task 09 embeds it; `tests/public-api.txt` guards it).
  It carries `timer_hz` so `restore(&state)` is self-contained, and stores `initial_count +
  count_at_arm + timer_arm_vns + timer_running + timer_pending` rather than a deadline — absolute
  deadlines are derived, so they survive restore unchanged (matching the `TimerQueue`/`VClock`
  discipline in INTEGRATION.md §4). The timer **mode** and **period** are derived from the
  LVT-timer entry and `count_at_arm`/divide/`timer_hz` (not duplicated), so there is no
  denormalized field that could desync. `version` = `LAPIC_STATE_VERSION` (3); `restore` rejects a
  mismatched version, `timer_hz == 0`, a pending timer with a zero count, a running-but-not-pending
  timer, or a running timer whose `count_at_arm` exceeds `initial_count`.
- **One write-mask table governs every register** (PR #38 systematic pass). A per-register
  guest-writable bit mask (`*_WRITE_MASK` / `lvt_write_mask`) is the single source of truth for
  which bits a guest may set per the SDM (Vol. 3A §11.5/§11.6, Figure 11-8) and the frozen
  contract; `mmio_write` stores `value & MASK` for every storage register, so **no register can
  ever hold a reserved bit**. The masks encode the per-LVT field differences exactly — notably the
  **Error LVT has no delivery-mode field** (only vector + mask), unlike Thermal/PerfMon, so its
  bits 8..=10 stay 0 (sharing their mask previously leaked reserved state through MMIO and the
  snapshot). DFR is the one exception to plain masking: its reserved bits 0..=27 read as 1.
- **Divide-config bit 2 is accepted but not stored** (PR #38 final review — a determinism fix).
  The divisor is selected by TDCR bits [3,1,0]; bit 2 is decode-ignored (`divide_value` masks it
  out). It used to be *stored* (`TDCR_WRITE_MASK` was `0xF`), so a guest that set bit 2 and one
  that didn't — behaviorally identical, same divisor — produced different `divide_config` in the
  register readback **and** in `LapicState`, hashing differently. The mask is now `0xB`: the
  write is still accepted (not an error, per deny-ignore), but bit 2 is dropped at storage, so the
  readback, the timer behavior, and the snapshot/hash all match the bit-2-clear write. Because the
  mask is the single source of truth, `restore` validation (`state_bits_canonical`) now also
  rejects a stored `divide_config` with bit 2 set as unreachable. Proven for all values by the
  `tdcr_write_mask_drops_ignored_bit` Kani harness and the `tdcr_bit2_dropped_not_stored` unit
  test; the snapshot-determinism proptest in `tests/snapshot.rs` covers the round trip end-to-end.
- **`restore` is a strict validation boundary** (PR #38 systematic pass). It accepts a
  `LapicState` **only if the MMIO write paths could have produced it**, else `InvalidState`. The
  enumerated invariants: current `version`, `timer_hz != 0`, every register's reserved bits clear
  (`state_bits_canonical`, using the *same* write-mask table), a pending count is non-zero, and
  `timer_running == timer_armable()` (counting iff pending && enabled && unmasked && supported
  mode — rejecting a fired one-shot marked running, or running-while-masked/disabled). An accepted
  state round-trips through `snapshot` exactly (restore never silently normalizes). Both
  directions are property-tested in `tests/registers.rs` against an *independent* SDM-literal
  validator (so the crate's masks can't drift unnoticed), and `lvt_write_masks_exclude_reserved`
  proves the LVT masks for all values in Kani.
- **APIC ID is read-only.** The SDM permits writing the xAPIC ID, but R1's single-vCPU,
  frozen-topology model pins it (CPUID initial APIC ID = 0). Modeling it read-only removes a
  mutable field that could diverge and matches the contract; documented as a deliberate
  simplification.
- **Arming is re-evaluated on every gating change** (TMICT, LVT-timer, SVR-enable). A write to
  Initial Count (re)starts the timer from `now` when the LVT timer is unmasked, the APIC is
  software-enabled, the mode is one-shot/periodic, and the count is non-zero; writing 0 disarms.
  A masked or software-disabled write stores `N` but does not arm yet — and crucially, a later
  **unmask** (LVT-timer write clearing bit 16) **or enable** (SVR write setting bit 8) re-arms it
  at that instant via the unified re-arm path (PR #38 re-review fix #2). Symmetrically, masking,
  disabling, or selecting an unsupported mode **cancels** a running timer (no stale deadline
  lingers); a vector-only LVT change keeps the existing `arm_vns` (does not restart the count).
  This "masked ⇒ not counting; unmask ⇒ start counting from the unmask instant" model differs
  from strict hardware (where the count runs under mask) but is observationally equivalent for
  our purposes — the timer's only effect here is the interrupt — and is simpler and deterministic.
- **A fired one-shot is not resurrected** (PR #38 final pass). The Initial Count register retains
  `N` after a one-shot expires (per the SDM), so `initial_count != 0` cannot by itself gate
  re-arming — otherwise a later SVR/LVT-timer write that left the timer enabled+unmasked would
  hit the re-arm path's arm branch and re-fire spuriously. A separate `timer_pending` flag
  tracks "count loaded & not consumed": set by a non-zero Initial Count write, **cleared when a
  one-shot fires**. `timer_armable` keys off `timer_pending`, so only a fresh Initial Count write
  re-arms a fired one-shot. (`timer_pending` is part of `LapicState`, which bumps to version 2;
  `restore` validates `pending ⟹ count != 0` and `running ⟹ pending`. The whole lifecycle —
  arm→fire→arbitrary SVR/LVT/TMICT/divide/advance interleavings — is checked against an
  independent reference model in the `timer_lifecycle_matches_reference` proptest, plus the
  `fired_oneshot_not_resurrected` Kani harness and a unit regression.)
- **One re-arm path; no register change is ever retroactive** (PR #38, the 6th timer bug). Every
  timer-affecting write — Initial Count, LVT-timer (mask/mode/vector), Divide Config, SVR enable —
  routes through a single `retime` (via `timer_config_write`, which captures the remaining count
  and divisor *before* applying the change). `retime` either: cancels (not armable), re-anchors
  from the current remaining when the **divisor changed** mid-count (so the new rate applies only
  going forward — a Divide-Config write reschedules, never rewriting history or firing in the
  past), keeps the anchor exactly when a still-armable change left the rate alone (mask→unmask /
  vector / mode — no rounding drift), or fresh-loads `initial_count` on a stopped→armable
  transition or TMICT load. This unifies the previously spread-out arming logic that produced six
  timer bugs. The arbitrary-interleaving `timer_lifecycle_matches_reference` proptest (Initial
  Count / LVT / Divide / SVR / advance) and the `tdcr_change_no_retroactive_fire` Kani harness
  pin it: remaining is monotonic non-increasing while only time advances, the vector fires exactly
  once per arm, and a config change never makes a not-yet-due timer fire.
- **TSC-deadline mode (0b10) and the reserved mode (0b11) hold the timer stopped.** R1 masks the
  TSC-deadline CPUID bit, so a cooperative guest never selects it; if one does, the timer simply
  never arms/fires (documented, never a panic).
- **`next_timer_deadline` returns `None` for an unrepresentable deadline.** When `arm_vns +
  period` overflows `u64` (a period beyond ~584 years of V-time), the deadline is unreachable —
  `advance_to` will never fire it. Reporting `None` rather than a clamped `u64::MAX` keeps a
  `vtime::TimerQueue` caller from looping on a due-but-never-firing timer (PR #38 re-review
  fix #3). The firing logic uses the **un-saturated** `u128` period (`timer_period_u128`) for the
  same reason: `elapsed >= period` correctly treats an overflowing period as never-due.
- **Self-IPI, including physical self-destination.** A fixed-delivery-mode ICR write targets self
  when its shorthand is `01` (self) or `10` (all-including-self), **or** shorthand `00` with a
  *physical* destination (ICR-high bits 24..=31) equal to our APIC ID (the common `0 == 0` case)
  or the physical broadcast `0xFF` — these raise the vector locally (PR #38 re-review fix #1).
  Shorthand `11` (all-excluding-self), a non-matching physical destination, and logical-mode
  no-shorthand destinations (not modeled — single vCPU) are no-ops. Non-fixed delivery modes
  (NMI/INIT/SIPI) are `vmm-core`'s to issue.
- **ESR models the send-illegal-vector error.** A fixed self-IPI with a reserved vector (`< 16`)
  sets ESR bit 5 (SDM Vol. 3A §11.5.3) instead of delivering; a guest write to the ESR clears the
  accumulated error state (the SDM's write-arm-then-read latch, approximated). Other ESR error
  classes are not modeled (the cooperative guest does not provoke them). ESR round-trips through
  snapshots.
- **`raise` vs the timer/self-IPI path.** Public `raise` rejects vectors `< 16`
  (`ReservedVector`); the internal IRR-set used by the timer and self-IPI does not (a reserved
  timer vector lands in IRR but is never deliverable — its priority class 0 can never exceed
  PPR's — so it is harmless rather than an error, and `advance_to`/`mmio_write` stay infallible
  beyond `BadOffset`).
- **PPR / priority.** Priority class is `vector >> 4`. PPR = TPR if `TPR[7:4] >= ISRV[7:4]`, else
  `ISRV & 0xF0` (SDM §11.8.3.1). `has_deliverable`/`take_interrupt` compare the highest IRR
  vector's class strictly against PPR's class; EOI clears the highest ISR bit. This yields LIFO
  interrupt nesting (gate 2 asserts it against a naive sorted-set model).
- **Mutation-testability** (quality-c, `cargo mutants --in-diff`). `divide_value` combines its two
  disjoint selector bit-fields with `+` rather than `|` — on disjoint bits `|`/`^` are
  equivalent (an unkillable mutant), whereas `+`→`-`/`*` change the divisor and are caught. The
  ICR self-IPI tests use a non-zero APIC ID, a non-fixed delivery mode, and a logical-mode
  destination so the bit-extraction shifts/masks are constrained (not just exercised with the
  `apic_id == 0`, fixed-mode happy path), and the ESR test asserts the literal `0x20` so a
  mutation of the `ESR_SEND_ILLEGAL_VECTOR` constant diverges from the expectation.
- **`advance_to` is idempotent and signals change.** It fires at most once per call per period
  boundary (periodic re-arm is closed-form), returns `true` iff state changed, and a repeat call
  at the same `now_vns` is a no-op returning `false`. The fire decision is gated on
  `elapsed >= period`, **not** `now >= deadline`: the deadline (`arm + period`) is a *saturating*
  add, so near `u64::MAX` it can clamp to `now` while fewer than one full period has elapsed —
  firing then would set the vector without advancing `arm_vns`, and a repeat call would re-fire
  forever (caller loops re-delivering). The elapsed-gate makes a period whose true deadline
  overflows `u64` simply never fire (~584 years of V-time is unreachable) and keeps the rearm
  idempotent even when `arm_vns` saturates. (PR #38 blocking fix; covered by the
  `advance_to_is_idempotent` proptest and the `advance_to_idempotent_at_saturation_boundary`
  Kani harness.)
- **Lints.** `Cargo.toml` does not use `[lints] workspace = true` because the crate registers the
  `kani` cfg (`[lints.rust] unexpected_cfgs`), which Cargo cannot combine with workspace lints;
  the `[lints.clippy] all = deny` table mirrors the workspace. Same shape as `vtime`.

## Deviations considered and rejected

- **Storing the firing deadline in `LapicState`.** Rejected: the deadline is a pure function of
  `arm_vns`/`N`/divide/`timer_hz`; storing it would denormalize and risk a desync, and the
  arm-based form is what makes the Current Count round trip exact for every `timer_hz`.
- **A background-ticked Current Count.** Rejected: ticking is nondeterministic relative to
  `now_vns` and pointless when the value is a closed form of elapsed V-time.
- **Modeling the count running while the LVT timer is masked.** Rejected for simplicity; since
  the interrupt is the timer's only observable effect, gating arming on unmasked-at-write is
  observationally equivalent and strictly simpler. Documented at `write_initial_count`.
- **`extern crate alloc` / `Vec` in the snapshot.** Not needed: `LapicState` is fixed-size arrays
  and scalars, so the crate is `alloc`-free. The `serde` dependency keeps the spec's
  `default-features = false, features = ["derive", "alloc"]` line, but the crate itself uses no
  `alloc` types.
- **A Miri job.** Not applicable: the crate has **no `unsafe`** and no `zerocopy`, so there is no
  undefined behavior for Miri to find. (If `unsafe` is ever added, the AGENTS.md review-bar rule
  requires wiring `cargo +nightly miri test -p lapic` into the `miri` CI job.)

## Known limitations

- The xAPIC ID is read-only (see above); a guest that rewrites it sees no effect.
- ESR error bits are not modeled (only the register's presence and clear-on-write).
- Single vCPU only: no inter-processor IPI delivery, no IOAPIC, no x2APIC (all structurally off
  per R1).
- Relocating the MMIO base (`IA32_APIC_BASE`) is `vmm-core`'s job; this crate is addressed purely
  by offset within the page.

## Acceptance gates

Standard gates (all green on macOS, rustc 1.94.1), with `--all-features` and with no features
(the core compiles and passes with `serde` **off**):

```
cargo build  -p lapic --all-features
cargo nextest run -p lapic --all-features   # 53 tests across lib + 5 suites, ≈ 0.5 s
cargo clippy -p lapic --all-features --all-targets -- -D warnings
cargo fmt    -p lapic -- --check
cargo deny check
```

Genuine `no_std` is verified by building the library for a bare-metal target with **no** `std`
at all, both feature sets — confirming `thiserror` (`default-features = false`) and `serde`
(`default-features = false` + `alloc`) stay `no_std`, which matters because task 09 embeds
`LapicState`:

```
cargo build -p lapic --target thumbv7em-none-eabi
cargo build -p lapic --target thumbv7em-none-eabi --features serde
```

Task gates map to the suites:

- **Gate 1 (timer round-trip)** — `tests/timer.rs`: arming-instant round trip (count == N for
  *every* `timer_hz`, incl. `24_000_000`), exact deadline `t0 + ceil(N·divide·1e9/timer_hz)` (or
  `None` when it overflows `u64`), count decay `N − floor(Δ·timer_hz/(divide·1e9))` (monotone, 0
  at/after deadline), one-shot fires once, periodic fires once per period with exact re-arm
  instants, `advance_to` idempotence at the `u64::MAX` boundary, the saturated-deadline `None`
  case never firing, masked-load-then-unmask arming, and a full-lifecycle model check
  (`timer_lifecycle_matches_reference`: arbitrary Initial-Count/LVT/Divide/SVR/advance interleavings
  vs an independent reference implementing the unified `count_at_arm`/`retime` model — comparing
  the fire-decision, deadline, and Current Count after every op, and asserting remaining is
  monotonic non-increasing while only time advances, the vector fires once per arm, and a config
  change never makes a not-yet-due timer fire retroactively; catches fired-one-shot resurrection,
  lost arms, and the mid-count divide re-anchor). 512 cases.
- **Gate 2 (delivery ordering)** — `tests/delivery.rs`: arbitrary `raise`/`take`/`eoi`/TPR
  sequences checked against a naive sorted-set + ISR model (highest deliverable above PPR; LIFO
  EOI nesting). 512 cases.
- **Gate 3 (snapshot round-trip)** — `tests/snapshot.rs`: arbitrary reachable state →
  `snapshot()` → `restore()` → observational equality (all offsets × 7 `now_vns` values,
  `next_timer_deadline`, `has_deliverable`); snapshot determinism across runs. 384 cases.
- **Register / restore validation** — `tests/registers.rs`: arbitrary `mmio_write` sequences
  never leave a reserved bit set in any register; an arbitrary `LapicState` is accepted by
  `restore` **iff** an independent SDM-literal validator says it's reachable+coherent, and an
  accepted state round-trips exactly; plus per-register reserved-bit rejection. 512 cases.
- **Gate 4 (Kani proofs)** — `src/device_proofs.rs` (see below).
- **Gate 5 (reset state)** — `tests/reset.rs`: SDM power-on values (software-disabled, LVTs
  masked, counts/priorities zero, ID/Version/DFR), and nothing deliverable until the guest
  software-enables the APIC.

## Formal proofs (Kani)

`#[cfg(kani)]` bounded-model-checking harnesses in `src/device_proofs.rs` (module `proofs`,
declared in `src/device.rs` so `use super::*` reaches the private helpers). They prove "never
panics / law holds for ALL inputs in the stated range" — strictly stronger than the proptest
sampling — via CBMC. They compile only under `cargo kani`; the normal build excludes them, and
`Cargo.toml`'s `[lints.rust] unexpected_cfgs` registers the `kani` cfg so the standard clippy
gate does not flag them. CI runs them in the Linux `kani` job (`.github/workflows/quality.yml`):
`cargo kani -p vtime && cargo kani -p lapic`.

**Where it ran.** Run locally with **Kani 0.67.0 / CBMC** on macOS (Apple Silicon, aarch64):
all **15 harnesses** report `VERIFICATION:SUCCESSFUL` (`Complete - 15 successfully verified
harnesses, 0 failures, 15 total`) in **≈ 42 s wall** total (`cargo kani -p lapic`; each harness
solves in single-digit seconds — the bounds below keep them CI-fast). CI reruns them on the
Linux box in the `kani` job. (Kani 0.67.0 matches the version `vtime`'s proofs were verified
with.)

### Why the bounds (the CBMC cost model)

Same lesson as `vtime::clock_proofs`: CBMC's cost is driven by **operator width and kind**, not
value-range `assume`s. A symbolic ÷ symbolic or × symbolic at `u128` width explodes the
instance, so the harnesses pin `timer_hz` to a concrete representative (25 MHz, the frozen
crystal — or 1 for the saturation harness) and iterate the divide config over **concrete**
values; each product then has a constant operand and each division a constant divisor, which
CBMC folds into cheap shift/reciprocal-multiply. Exact-equality across a `u128` divide is far
costlier than an inequality, so the exact harnesses bound the symbolic `N`/`Δ` to 12 bits (enough
to drive every rounding/carry path; larger operands only repeat the arithmetic), while the
saturation harness keeps `N` full-`u32` because its `÷ 1` is trivial. The divide-config decode is
proven for **all** `u32` inputs separately (`divide_value_total`), so the concrete-divisor
harnesses compose to "for every legal divisor".

### Harness catalogue

| Harness | Proves | Bound / regime |
|---|---|---|
| `divide_value_total` | divide decode never panics, returns one of the 8 legal divisors | symbolic `u32` config |
| `period_never_panics_any_count` | period never panics/overflows for **any** count or divisor | symbolic `u32` `N` + `u32` config, `timer_hz = 1` |
| `huge_period_reports_no_deadline` | a period exceeding `u64` makes `next_timer_deadline` return `None` (not a clamped `u64::MAX`) | ÷128, `timer_hz = 1`, concrete large `N` |
| `period_exact_ceil` | period == exact `ceil(N·divide·1e9/timer_hz)`; ceiling covers ≥ N ticks | 25 MHz, ÷16, `N ∈ [0,2¹²)` |
| `current_count_round_trips_at_arm` | count == `N` **exactly** at the arming instant (the headline round trip) | 25 MHz, **all 8 divisors**, full `u32` `N` |
| `current_count_exact_decay` | count == `N − floor(Δ·timer_hz/(divide·1e9))` | 25 MHz, ÷16, `N,Δ ∈ [0,2¹²)` |
| `current_count_monotone` | count is non-increasing in elapsed V-time | 25 MHz, ÷2, symbolic `Δ1 ≤ Δ2` |
| `advance_to_idempotent_at_saturation_boundary` | a repeat `advance_to` at the same `now_vns` is a no-op at the `u64::MAX` deadline-saturation boundary (PR #38) | `now = u64::MAX`, periodic ÷2, `arm` within 3 periods of `u64::MAX` |
| `fired_oneshot_not_resurrected` | a fired one-shot (pending cleared, count still non-zero) is not re-armed by the gating re-arm path — no deadline, no fire (PR #38) | enabled/unmasked one-shot, symbolic count + `now` |
| `tdcr_change_no_retroactive_fire` | a mid-count divide change reschedules from the current remaining — preserved count, no immediate fire, future deadline (PR #38, 6th bug) | one-shot ÷2→÷128, N=1000, `now` within first period |
| `tdcr_write_mask_drops_ignored_bit` | the divide-config write mask drops the decode-ignored bit 2 (never stored) while leaving the decoded divisor unchanged, for **any** value — closes the snapshot/hash determinism gap | symbolic `u32` value |
| `lvt_write_masks_exclude_reserved` | every LVT write mask drops the RO delivery-status/remote-IRR bits, and Error excludes delivery-mode, for **any** value (PR #38) | symbolic `u32` value, all 6 LVT indices |
| `vec_index_in_bounds` | set/clear/highest round-trip without OOB for **any** `u8` vector | symbolic `u8` |
| `highest_vec_correct` | `highest_vec` total; returned vector is set; class ≤ 15; no OOB | symbolic `[u32;8]` |
| `ppr_and_delivery_total` | PPR class is a 4-bit value; `has_deliverable`/`take_interrupt` never panic and agree | symbolic TPR/ISR/IRR |
