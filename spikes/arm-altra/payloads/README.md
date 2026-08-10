# `payloads/` — arm64 oracle payloads + minimal bare-metal runtime

**UNTESTED ON SILICON.** Everything here has been built and booted under
`qemu-system-aarch64` (TCG) only. TCG proves **liveness and protocol**; it proves
nothing about `BR_RETIRED` counts, PMIs, or skid. Those are stage AA-1's, on real
Neoverse N1 (`docs/ARM-ALTRA.md`).

Two cargo workspaces meet here:

- **this one** (`aarch64-unknown-none`, `no_std`): `runtime/` (the bare-metal
  runtime) and `oracles/` (the payload bins).
- **`../oracle-model`** (shared, builds for both host and target): the analytical
  taken-branch oracle. It is the single definition of every payload parameter and
  every expected count, compiled into *both* the payloads and the host harness, so
  the asm and the model cannot drift.

## The counting window — the whole idea

`docs/ARM-ALTRA.md` §Evidence integrity #5 forbids judging count-exactness by
comparing one PMU reading against another (circular). Counts are judged against
payloads whose **taken-branch count is known by construction**. Each payload's
counted body is bracketed by two MMIO console stores:

```
        <drain the UART transmitter>      ; the drain's poll back-edge is OUTSIDE
        strb  MARK_BEGIN, [uart]          ; --- window opens (harness samples here)
__win_<name>_start:
        <hand-written counted body>       ; every branch here is explicit
__win_<name>_end:
        strb  MARK_END,   [uart]          ; --- window closes (harness samples here)
```

Under the KVM harness each mark store is an MMIO exit at which the harness samples
`BR_RETIRED`. So the counted region contains *exactly* the body — no boot code, no
UART poll loop, no compiler-generated setup. The body is **hand-written asm**
(`oracles/src/asm/*.s`) precisely so its taken-branch count is a property of the
instructions, not of what the compiler chose to emit; the Rust half only does
setup, reporting and the in-guest self-checks.

Two disciplines make the window branch-exact, and both are load-bearing:

1. **Drain before, not during.** The transmitter is drained (`FR.BUSY == 0`)
   *before* `MARK_BEGIN`. If the mark store had to wait for the FIFO, that wait's
   back-edge would be a wall-clock-dependent taken branch inside the window. After
   the drain nothing is written until `MARK_BEGIN`, so `MARK_END` needs no poll at
   all. (The PL011 FIFO is disabled for the same reason; `runtime/src/uart.rs`.)
2. **Exception handlers inline in the vector slot.** The three payloads that take
   exceptions on purpose (`svc`, `exception-abort`, `wfi-idle`) install their own
   vector table with the handler placed directly in the slot, so the exception
   path contributes **zero branch instructions** beyond the `ERET`. The harness's
   `arm-scan` verifies each handler is exactly one `ERET` — a shared,
   compiler-generated dispatcher would contribute unknown branches and the count
   would stop being known by construction.

## What is known, and what is measured

The count of a window decomposes as

```
measured = certain_taken                       (exact, in oracle-model)
         + reported_taken                       (STXR/seqlock retries, counted in-guest)
         + w_entry·entries + w_eret·erets        \ the four UNKNOWN weights,
         + w_svc·svcs      + w_wfi·wfis          / measured on silicon by AA-1
         + window_offset                        (also an AA-1 unknown; x86's was n+2)
```

The four weights and the offset are **spike deliverables, not defaults**. The
oracle's `Weights` type has no `Default` and no invented values: pre-silicon a
checker cannot obtain one, and refuses to check counts rather than guess (task 109
"no invented constants"). The payload set is designed so the weights are separately
identifiable from measurements — see `../oracle-model/src/lib.rs` §Identifiability.

## The payloads

| Payload | Class / stage | Counted body | Ambiguity terms |
|---|---|---|---|
| `ident` | AA-0/AA-6 witness | — (no window) | reports MIDR/LSE/ECV/PMUVer/SVE from inside the guest |
| `straight-line` | AA-1 lowest density | 64 ALU insns + 1 back-edge / trip | none |
| `branch-dense` | AA-1 highest density | 7 data-dependent branches / trip (TBZ/TBNZ/CBZ/B.cond) | none |
| `svc` | AA-1/AA-2 syscall | `SVC #0` / trip | entry, eret, **svc** |
| `exception-abort` | AA-1 exception | translation fault (EC 0x25) / trip | entry, eret |
| `wfi-idle` | AA-1 idle | mask→SGI-pending→WFI→unmask / trip | entry, eret, **wfi** |
| `llsc-atomics` | AA-4(a) hazard | `LDXR`/`STXR` increment / trip | reported: STXR retries |
| `lse-atomics` | AA-4(b) answer | `LDADD` increment / trip | none |
| `clock-page` | AA-5 | seqlock read of the pvclock page / trip | reported: retries (must be 0) |
| `aa4-self-modify` | AA-4 level-3 proof fixture | dedicated page changes `mov x0,#1` → `mov x0,#2`, then executes again | no measurement window; VMM audits pre-store and rescan hashes |

The first nine are the oracle/model set. `aa4-self-modify` is deliberately outside
`ALL_PAYLOADS`: it measures no branch count and exists only to make the execute-guard's
write-before-modification and rescan transitions non-vacuous. Its target occupies one complete,
page-aligned executable page; the host proof pins both instruction encodings and both full-page
hashes.

`straight-line` and `branch-dense` (zero ambiguity) pin `window_offset` from two
densities; `exception-abort` yields entry+eret; `svc` minus `exception-abort`
isolates `w_svc`; `wfi-idle` minus `exception-abort` isolates `w_wfi`. Five
equations, four unknowns — over-determined, and the residual is itself evidence.

Each payload reports whether the harness published its params/pvclock pages
(`mode=managed`) or it self-seeded them (`mode=self-seeded`, the TCG case), so a
harness that forgot to publish a page cannot be mistaken for one that did
(§Evidence integrity #4).

## The accumulators — the strongest thing TCG can say

`branch-dense` and `straight-line` return an accumulator that the oracle model
predicts exactly. For `branch-dense` each branch adds a distinct weight on its
*not-taken* path, so a matching accumulator proves every one of the seven
predicates evaluated as the model says, on every trip — the branch logic and the
PRNG agree bit-for-bit between the asm and the model. `smoke.sh` checks this on
every run, and `../oracle-model/tests/tcg_observed.rs` pins the TCG-observed values.
This validates the *predicates*; it says nothing about whether hardware counts
those branches, which is silicon's alone.

## Build & smoke

```sh
# targets: rustup target add aarch64-unknown-none
cargo build --release                        # nine oracle ELFs + the AA-4 proof fixture
./smoke.sh                                    # boot each under TCG, diff structure, propagate RC
```

`smoke.sh` builds the payloads, runs the oracle-model self-checks + TCG accumulator
pins, verifies every window's branch sequence against the model (`arm-scan
windows`), then boots each payload twice under `qemu-system-aarch64 -cpu
neoverse-n1` and diffs the normalized console against `golden/`. Every constituent
RC propagates: a nonzero payload exit, a timeout, or a golden mismatch fails the
whole script. There is no done-marker success path.

The expected-count manifest is `expected/expected-counts.json`, regenerated with
`(cd ../harness && cargo run --bin arm-scan -- manifest)` and kept current by the
`manifest_current` generator test.

## Toolchain notes

- `runtime/` is the only place `unsafe` lives (bare-metal MMIO, system registers,
  raw asm) — granted for this directory by task 109 §Constraints. The harness's
  scanning/checking logic has none.
- `-cpu neoverse-n1` under TCG models the target part's ID registers, so `ident`'s
  self-report is representative in shape (not in the counter facts TCG cannot give).
- The QEMU `virt` memory map is matched by the harness's modelled GPAs (PL011 at
  `0x0900_0000`, GICv3 at `0x0800_0000`, RAM at `0x4000_0000`), so the payloads are
  byte-identical across TCG and the KVM harness.
