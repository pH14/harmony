# Task 31 — exhaustive allow-stateful MSR round-trip sweep

> **ACTIVE NOW · runs in parallel with task 30 (Linux boot).** Extends task 18's instruction sweep.
> Integrator directive (2026-06-25): make the allowed-MSR sweep cover **every** allow-stateful MSR,
> not a hand-picked 4 — and do it now, because it **de-risks the Linux boot**: the untested MSRs are
> exactly the ones Linux writes during early boot.

Read `tasks/00-CONVENTIONS.md`, `tasks/18-instruction-sweep.md`, `docs/cpu-msr-contract.toml`, and
`consonance/acceptance-suite/payloads/README.md` first.

## Why

Today the `msr-allowed` payload round-trips only **4 of ~43** allow-stateful MSR indices
(`MSR_ROUNDTRIP_SAFE` in `consonance/acceptance-suite/payloads/contract-data/src/lib.rs` = `SYSENTER_CS 0x174`,
`STAR 0xc0000081`, `FS_BASE 0xc0000100`, `KERNEL_GS_BASE 0xc0000102`). The contract's
**allow-stateful** set is much larger and includes the MSRs Linux configures on the boot path:
`EFER 0xc0000080`, `LSTAR 0xc0000082`, `CSTAR 0xc0000083`, `SYSCALL_MASK 0xc0000084`,
`SYSENTER_ESP/EIP 0x175/0x176`, `GS_BASE 0xc0000101`, `TSC_AUX 0xc0000103`, `CR_PAT 0x277`, and the
MTRR block (`0x200–0x20f`, `0x250`, `0x258–0x259`, `0x268–0x26f`, `0x2ff`). Proving every one of
these reads-back-what-was-written **per the contract** before task 30's kernel relies on it turns a
class of "Linux boot mysteriously diverged" failures into a cheap, isolated unit of coverage.

(The instruction sweep's other gaps — 39 uniform-`#UD`/host-absent/permit-native instructions — are
**deliberately and documentedly** omitted in `consonance/acceptance-suite/payloads/README.md` and stay omitted. This task
is **only** the allow-stateful MSR completeness gap.)

## Scope

1. **Generate the round-trip set from the contract, not a hand list.** Replace the hand-picked
   `MSR_ROUNDTRIP_SAFE` with the **full allow-stateful set** derived from `docs/cpu-msr-contract.toml`
   (the same generation path `contract-data` already uses for `allow-fixed`/CPUID). Every allow-stateful
   MSR index the contract lists is swept — adding a new allow-stateful row to the contract must
   automatically extend the sweep (a `contract-data`/codegen test asserts the sweep set == the
   contract's allow-stateful set, so a future divergence fails loudly).
2. **Round-trip each with a contract-legal value.** `msr-allowed` writes a known value, reads it back,
   asserts equality, and reports each `(index, value)` on the report channel for the box O2 digest.
   **Per-MSR legality matters** — pick write values that are valid for the architecture so the test
   exercises *contract* behavior, not a `#GP` you mis-attribute:
   - canonical addresses for `FS_BASE`/`GS_BASE`/`KERNEL_GS_BASE`/`LSTAR`/`CSTAR`/`SYSENTER_EIP/ESP`
     (bit 47 sign-extended);
   - `EFER`: only toggle bits the contract/guest already permits (`SCE`); **do not** clear `LME`/`LMA`
     under a 64-bit guest (that faults) — read-modify-write the permitted bit(s) and restore;
   - MTRRs / `CR_PAT`: write values with reserved bits clear and valid memory-type encodings;
   - `TSC_AUX`/`STAR`/`SYSCALL_MASK`: full 32/64-bit round-trip as the width allows.
   Document the chosen value per MSR (a table) so the legality reasoning is reviewable. Restore any MSR
   whose live value the rest of the payload depends on.
3. **Keep determinism.** The reported stream must be a pure function of the run (no host-derived
   values); values are fixed constants or RMW of contract-fixed reads.

## Acceptance gates

1. **Completeness, mechanized.** A test asserts the swept allow-stateful index set **equals** the
   contract's allow-stateful set (parsed from `cpu-msr-contract.toml`) — so the sweep can never again
   silently cover a subset. No index in the contract's allow-stateful set is unswept.
2. **Round-trip correctness.** For every swept MSR, write→read-back→assert-equal passes (on the box,
   patched/stock as the contract row requires); a mismatch or an unexpected `#GP` is a loud failure.
3. **O2 golden.** The `msr-allowed` observable digest golden (`consonance/acceptance-suite/golden/msr-allowed.digest`) is
   re-blessed on the box to include the expanded report stream; the box gate
   (`consonance/vmm-core/tests/box_corpus.rs`, item `msr-allowed`) passes against it.
4. **No contract change.** This task does **not** alter `cpu-msr-contract.toml` or its `§6` hash — it
   only widens *coverage* of the already-frozen allow-stateful set. If a contract MSR turns out to be
   mis-classified (e.g. not actually round-trippable on the box), raise it to the integrator rather
   than editing the contract or silently skipping the MSR.
5. Standard gates green; the generated-from-contract list keeps the build hermetic (no network).

## Non-goals

The 39 documented instruction omissions (stay omitted); exhaustive default-deny MSR enumeration
(the 1043-index space — the 11-sample stays representative; deny shares one `#GP` disposition);
changing the contract or its hash; box-only "write-is-ignored" deny semantics (already covered).
Touch only `consonance/acceptance-suite/payloads/msr-allowed`, `consonance/acceptance-suite/payloads/contract-data`, and the relevant golden +
its box-gate assertion — not the VMM crates.
