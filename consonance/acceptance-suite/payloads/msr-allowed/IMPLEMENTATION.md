# Task 31 — exhaustive allow-stateful MSR round-trip sweep: implementation notes

Extends task 18's `msr-allowed` from a hand-picked **4-of-41** allow-stateful
subset to **every** allow-stateful MSR in `docs/cpu-msr-contract.toml`. Touches
only `consonance/acceptance-suite/payloads/msr-allowed`, `consonance/acceptance-suite/payloads/contract-data`, the README
coverage doc, and the `msr-allowed` O2 golden. The contract and its §6 hash are
unchanged (gate 4): this widens *coverage*, not the frozen surface.

## What landed

- **`contract-data`**: a contract-legal round-trip plan API —
  `RoundtripKind` (`Exact` | `Toggle`), `MsrRoundtrip`, `roundtrip_value(index)`
  and `canonical_addr(index)`. `roundtrip_value` supplies a per-MSR *legal* write
  value; the swept *set* is the generated `MSR_ALLOWED_STATEFUL` (so it is the
  contract's, not a hand list). The old hand-picked `MSR_ROUNDTRIP_SAFE` is gone.
- **`msr-allowed` payload**: the round-trip loop now iterates
  `MSR_ALLOWED_STATEFUL` (41 indices) instead of the 4-element list. Per index it
  reads the live value, writes `roundtrip_value`'s value (verbatim, or `orig ^
  mask` for `Toggle`), reads back, **restores `orig` before any failure path**,
  asserts read-back == written (loud `FAIL` otherwise), and reports
  `(index, written)` on the report channel. The serial banner is unchanged, so
  `consonance/acceptance-suite/golden/msr-allowed.txt` is unchanged.
- **Three mechanized gates in `contract-data`**:
  - `sweep_set_equals_contract_allow_stateful` (**gate 1**): a *second, minimal*
    parser re-reads `cpu-msr-contract.toml` and asserts the swept set equals the
    contract's allow-stateful set. A future allow-stateful row that the sweep
    misses fails here loudly — the sweep can never silently cover a subset.
  - `roundtrip_values_are_legal` (**gate 2 support**): asserts every chosen write
    value is architecturally legal for its index (canonical where an address is
    required; valid memory-type encodings and clear reserved bits for the
    MTRR/PAT block; `MAXPHYADDR=39`), so a box round-trip miss is a real contract
    problem, never a `#GP` from an illegal probe value.
  - `roundtrip_values_are_exactly_pinned` (**mutation oracle**): an independent
    literal table of the exact `(value, kind)` for all 41 indices — the legality
    test only checks *properties*, so a value mutated to another legal value would
    slip past it. This pins the bytes; a newly-swept index with no pin is a loud
    `panic`.

## Per-MSR write values (the legality table)

The swept set, in contract order, and why each write is legal. Full reasoning is
in the `roundtrip_value` doc-comment.

| MSR(s) | index | write | legal because |
|--------|-------|-------|---------------|
| SYSENTER_CS | 0x174 | `0x08` | 32-bit selector field |
| SYSENTER_ESP / _EIP | 0x175 / 0x176 | `canonical_addr` | canonical linear address |
| variable MTRR PHYSBASEn | 0x200,0x202…0x20e | `(idx<<12)\|WB(6)` | base ≤ MAXPHYADDR, valid type, reserved clear |
| variable MTRR PHYSMASKn | 0x201,0x203…0x20f | `(idx<<12)\|Valid(1<<11)` | mask ≤ MAXPHYADDR, Valid set, reserved clear |
| fixed MTRR (64K/16K/4K) | 0x250,0x258,0x259,0x268–0x26f | `0x0605040100040506` | non-uniform valid memory-type bytes (NOT QEMU's all-WB/WP/0 defaults) |
| MTRRdefType | 0x2ff | `0x0c04` | `E`\|`FE`\|type=WT, reserved bits clear (WT not WB → != QEMU default `0xc06`) |
| CR_PAT | 0x277 | `0x0004070600040706` | 8 valid PAT type bytes; reset PAT with WT↔UC- swapped (NOT the reset value) |
| EFER | 0xc0000080 | **toggle** `SCE` (bit 0) | RMW; `LME`/`LMA`/`NXE` preserved under the 64-bit guest |
| STAR | 0xc0000081 | `0x0023001b00000000` | freely-writable SYSCALL/SYSRET selectors |
| LSTAR / CSTAR | 0xc0000082 / 83 | `canonical_addr` | canonical linear address |
| SYSCALL_MASK (FMASK) | 0xc0000084 | `0x00047700` | 32-bit RFLAGS mask (high dword zero) |
| FS_BASE / GS_BASE / KERNEL_GS_BASE | 0xc0000100/101/102 | `canonical_addr` | canonical linear address |
| TSC_AUX | 0xc0000103 | `0xc0ffee03` | 32-bit aux (high dword must be zero) |

`canonical_addr(index) = 0xffff_8000_0000_0000 + (index << 12)`: high-canonical
(bit 47 sign-extended), distinct per index. A `u32` index only sets bits 12..=43,
so bits 63:47 stay all-ones — canonical for any index. (`+`, not `|`, composes the
two disjoint fields; see *Mutation testing*.)

**Non-vacuous round-trip (PR #57 review).** A round-trip only proves the `WRMSR`
took effect if the written value differs from the MSR's live value — otherwise a
*silently dropped* write (e.g. an MSR mis-classified as deny-ignore-write) would
read back the value anyway and pass. So every value is chosen distinct from the
live value **in both environments**, which differ: the box (KVM) zero-inits the
MTRR/PAT block, but **QEMU TCG installs a firmware-style MTRR config** —
`PHYSBASE0=0x80000000`, fixed MTRRs all-WB / all-WP / 0, `MTRRdefType=0xc06`, and
the reset PAT. This review caught three collisions with the QEMU defaults
(`0x250`/`0x258` all-WB, `0x2ff`=`0xc06`) plus CR_PAT == the reset PAT. Fixes:
CR_PAT → `0x0004070600040706` (reset PAT, WT↔UC- swapped); fixed MTRRs →
`0x0605040100040506` (a non-uniform spread, != all-WB/WP/0); MTRRdefType →
`0xc04` (WT, != `0xc06`). The variable MTRRs already differ from QEMU's
`PHYSBASE0`/`PHYSMASK0`, and every non-MTRR index is non-zero vs. its zero reset
(EFER toggles a bit, so `written = orig ^ 1 != orig` by construction). The
payload also **asserts `written != orig`** per index — the audit is now a runtime
invariant: a future live-equal value fails loudly (under QEMU *and* the box), not
silently. (The QEMU live values were dumped with a throwaway diagnostic build.)

**EFER detail.** The boot shim sets `EFER.LME` then `CR0.PG`, so at payload entry
`EFER = LME|LMA = 0x500` (`SCE=0`). The toggle writes `0x501`; KVM masks the
written `LMA` and re-ORs the live `LMA`, and `SCE` is freely settable, so the
read-back is `0x501 == orig ^ 1`. We never clear `LME`/`LMA` (that faults under a
64-bit guest). The reported `written` for EFER is therefore the deterministic
`0x501`.

## Determinism (gate 5 / scope item 3)

The report stream is a pure function of the run: every reported `written` is a
fixed contract-derived constant, except EFER's `0x501`, which is an RMW of a
contract-fixed boot value (`0x500`) — deterministic across runs and identical on
the box. No host-derived value enters the stream. Every MSR is restored to its
`orig` immediately after read-back, so the round-trip leaves no state the rest of
the payload (or O1 replay) depends on.

## Acceptance-gate status

- **Gate 1 (completeness, mechanized)** — `sweep_set_equals_contract_allow_stateful`
  passes; 41/41 allow-stateful indices swept. ✔ (local)
- **Gate 2 (round-trip correctness)** — all 41 round-trip in-guest to a clean
  `PASS` under stock QEMU TCG (the Part-A gate), and `roundtrip_values_are_legal`
  proves every write satisfies the reserved-bit / canonical / memory-type rules
  KVM enforces. ✔ under QEMU; the patched-box confirmation rides the box gate
  below.
- **Gate 3 (O2 golden)** — the report stream expanded, so the box-captured
  `consonance/acceptance-suite/golden/msr-allowed.digest` is now **unblessed** (placeholder content, the
  box gate's documented "not captured" state). **Re-bless on the box** is required
  (see below). ✗ pending box.
- **Gate 4 (no contract change)** — `cpu-msr-contract.toml` and its hash untouched. ✔
- **Gate 5 (standard gates, hermetic)** — `cargo build`/`clippy -D warnings`/`fmt
  --check`/`nextest` green for `contract-data` and `msr-allowed`; `payloads/run-tests.sh`
  green (all 15 payloads, both runs byte-identical); no new dependencies; the
  contract list is generated at build time (no network). ✔ (local)

## Mutation testing

`cargo mutants --in-diff` over the diff's host-testable logic (`contract-data`'s
`canonical_addr` + `roundtrip_value`): **27 mutants — 26 caught, 1 unviable, 0
missed**. The unviable one is `roundtrip_value -> Default::default()` (the struct
has no `Default`, so it does not compile). Two notes:

- The exact-pin test is what closes the gap: it kills the value-, operator-,
  match-arm- and even/odd-guard mutants that the property-only legality test let
  survive.
- The disjoint bit-fields are composed with `+`, not `|` (`canonical_addr`, the
  MTRR PHYSBASE/MASK values). For disjoint operands `+ == |`, but `^`/`&` (the
  mutants `|` would generate) are *also* equal to `|` here — i.e. provably
  equivalent, unkillable mutants. `+` yields the same value while its mutants
  (`-`, `*`) differ, so they are killed by the pin and the count reaches a true 0.

Run it (host target; the workspace defaults to `x86_64-unknown-none`, and
`build.rs` reads `docs/` outside the crate, so use `--in-place`):

```sh
cd consonance/acceptance-suite/payloads
git diff --relative=consonance/acceptance-suite/payloads <base>...HEAD -- consonance/acceptance-suite/payloads/contract-data/src/lib.rs > /tmp/cd.diff
CARGO_BUILD_TARGET="$(rustc -vV | sed -n 's/^host: //p')" \
  cargo mutants --in-place --in-diff /tmp/cd.diff --package contract-data
```

`msr-allowed/src/main.rs` is a freestanding `#![no_main]` payload: it cannot be
built or run on the host, so it is out of host mutation scope (its behavior is
gated by `run-tests.sh` under QEMU and the box gate) — consistent with the
project's `main.rs`-exclusion mutants convention. CI's `mutants` job runs on the
**root** workspace, which excludes the standalone payload workspace, so this was run manually.

## What the integrator must do on the box

The box is currently on **stock** KVM (`kvm 1396736`); the box O2 gate needs the
**patched** module (`KVM_CAP_X86_DETERMINISTIC_INTERCEPTS`). On this branch, on
the box (patched modules loaded per `consonance/vmm-backend/kvm-patches/BUILD.md`, then
reverted to stock 1396736), CPU-pinned per `docs/BOX-PINNING.md`:

```sh
cd consonance/acceptance-suite/payloads && cargo build --release            # build the expanded payload
cd ../..
# re-bless the expanded report-stream digest, review, commit:
DETCORPUS_BLESS=1 taskset -c 2 cargo test -p vmm-core --test box_corpus -- --ignored --nocapture
git diff consonance/acceptance-suite/golden/msr-allowed.digest
# verify the gate passes against the blessed digest (no env var):
taskset -c 2 cargo test -p vmm-core --test box_corpus -- --ignored --nocapture
```

This confirms gate 2 on real `det-cfl-v1` hardware and produces the gate-3 golden.
The box-gate assertion itself (`consonance/vmm-core/tests/box_corpus.rs`, item
`msr-allowed`) needs no change — it reads the golden from the manifest.

## Deviations considered and rejected

- **Iterate a generated set vs. a hand table.** Chose to sweep the generated
  `MSR_ALLOWED_STATEFUL` directly (a new contract row is swept automatically) and
  keep the *values* in `roundtrip_value`, rather than a hand-authored
  `(index, value)` table that a contract row could silently outgrow. The
  completeness test still guards the set.
- **Reporting the read-back vs. the written value.** Report `written`
  (constant / RMW-deterministic). The round-trip *correctness* is the in-guest
  `FAIL`; reporting the written value keeps the digest a pure function of the
  contract, not of any read path.
- **Computing the digest offline.** Rejected — `observable_digest` is a vmm-core
  internal over the report stream + serial; the blessed digest must come from a
  real patched-box run (the point of box-blessing). Hence the placeholder.
- **High-canonical vs. low-canonical addresses.** Used high-canonical
  (`0xffff_8000…`) values to exercise bit-47 sign-extension; both are equally
  legal (the rule is identical on QEMU and KVM), and high-canonical mirrors real
  kernel addresses (LSTAR, per-CPU GS_BASE).

## Known limitations

- The non-allow-stateful MSR omissions (emulate-vtime, deny-gp, deny-ignore-write)
  stay omitted per the task non-goals — this task closes only the allow-stateful
  completeness gap. See `README.md` for the per-disposition coverage map.
- A future allow-stateful MSR with mandatory non-zero/reserved bits would hit the
  `roundtrip_value` default (write 0) and fail the box round-trip loudly — the
  signal to add an explicit arm (or escalate a mis-classification, never edit the
  contract or silently skip).
