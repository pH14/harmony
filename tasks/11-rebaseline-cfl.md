# Task 11 — Re-baseline the CPU/MSR determinism contract to the box: `det-skx-v1` → `det-cfl-v1` (contract-version 3)

Read `tasks/00-CONVENTIONS.md` first. This task re-baselines the **frozen CPU/MSR determinism
contract** from the synthetic Skylake-SP model (`det-skx-v1`) to the actual determinism box — an
**Intel Core i9-9900K (Coffee Lake-S, `06_9e_0c`, microcode `0xf8`)** — and bumps the contract to
**version 3 / baseline `det-cfl-v1`**.

> **Why this exists (ratified, do not renegotiate):** the box is a 9900K, not a Skylake-SP. Running
> the SKX-frozen contract on it means native non-trapping instructions (XSAVE layout,
> MAXPHYADDR-dependent paging, FPU) reflect Coffee Lake while the guest is told it is SKX — the
> exact cross-host divergence the §1.1 host-assert exists to refuse. Task 15's host-assert
> **correctly** fails 3/13 on the box (`family-model-stepping`, `host-microcode-rev`,
> `maxphyaddr-min`). The integrator ruled (2026-06-23/24): **re-baseline to the box CPU** so M1/M2
> can run bit-identical on real hardware. This must land **before** M1/M2 box bring-up and **before**
> the harmony rename (task 90).

## This is a TARGETED RE-BASELINE, not a redesign — hard rule

Preserve **every deliberate design decision** in `docs/CPU-MSR-CONTRACT.md`: the §1 default-deny
posture, the class taxonomy (§3), the synthetic single-socket/single-core/single-thread topology,
the §6 canonical-form grammar, the threat model, the gate structure. **Change only what the host
CPU forces to change**, and update the *narrative* only where a host fact actually changed (most
importantly: TSX is **present-but-always-aborting** on SKX but **physically absent** on the 9900K).
Any change not traceable to "the 9900K differs from SKX here" is out of scope — raise it as a
`[question]` in the PR, do not fold it in silently. The contract is the project's determinism
**drift anchor**; a wrong synthetic value silently corrupts every future determinism claim, so the
bar here is *correctness of every emitted constant*, evidenced against the box.

## Source of truth = the box. Dump it, don't guess.

The normative inputs are the **actual 9900K** CPUID/MSR values, captured from the box
(`ssh <det-box>`), **not** SDM defaults or guesses. Capture and commit the raw evidence under
`docs/fragments/cfl-baseline/` so the review can audit every changed constant against it:

```sh
# on the box (install cpuid / msr-tools if absent; rdmsr needs root + `modprobe msr`)
ssh <det-box> 'cpuid -1 -r'                       > cpuid-raw.txt      # all leaves, raw regs, this CPU
ssh <det-box> 'cpuid -1'                           > cpuid-decoded.txt  # decoded, for cross-checking
ssh <det-box> 'sudo rdmsr -a 0x10a; sudo rdmsr 0x8b; cat /proc/cpuinfo | grep -m1 microcode'
ssh <det-box> 'cat /sys/devices/system/cpu/cpu0/microcode/version'
ssh <det-box> 'lscpu; uname -r'
```

Pin the microcode revision the **kernel records** (`0xf8` expected — confirm), since that is exactly
what the §1.1 `host-microcode-rev` assert reads (`hostassert.rs::read_microcode_rev`).

## Change-set — enumerate, derive each from the box dump

Edit `docs/CPU-MSR-CONTRACT.md` (**source of truth — md wins over toml**) and regenerate
`docs/cpu-msr-contract.toml` to match. The two must stay consistent (the §6 grammar is mechanical).
Work through **all** of the following; the obvious four are only the start — Coffee-Lake-client vs
Skylake-SP-server differences ripple:

**Header / identity (`[contract]` + §1.1 + §2 intro):**
- `version = 2` → `3`; `cpuid-baseline = "det-skx-v1"` → `"det-cfl-v1"`. Rename `det-skx-v1` →
  `det-cfl-v1` throughout the prose, and re-style the "Skylake-SP" framing to "Coffee Lake-S
  (client, single-socket)".
- §1.1 host-assert expected records: `family-model-stepping` `06_55_04` → `06_9e_0c`;
  `host-microcode-rev` `0x0200005e` → `0xf8`; `maxphyaddr-min` `46` → `39`. (`mxcsr-mask 0xffff`
  and the host-absent-instruction set are expected to carry over — **confirm `MXCSR_MASK == 0xFFFF`
  on the box** via FXSAVE; it is host-specific.)

**CPUID leaves (§2 table + every `[[cpuid.entry]]`) — derive from `cpuid-raw.txt`:**
- **Leaf 1 EAX** `0x00050654` → `0x000906ec` (confirm: stepping 0xc, model 0x9e, family 6).
- **Leaf 1 ECX/EDX** feature bits — re-derive; CFL client differs from SKX server (and keep the
  synthetic overrides the contract deliberately imposes, e.g. cleared HTT/topology, `osxsave` dyn).
- **Leaf 7.0 EBX/ECX/EDX** — the big one: the 9900K has **no AVX-512** (EBX[16] AVX512F = 0 and the
  whole AVX-512 family clear), **no RTM** (EBX[11] = 0) and **no HLE** (EBX[4] = 0), **no SHA**
  (EBX[29] — confirm), **no RDPID** (ECX[22] — SKX baseline already cleared it), **no SERIALIZE**,
  **no WAITPKG**. Re-derive the whole leaf from the box. Keep EDX[29]=ARCH_CAPABILITIES-enumerated
  consistent with the §3.9 0x10a row (see below).
- **Leaf 4** (deterministic cache parameters) — CFL client cache topology ≠ SKX server; re-derive
  every subleaf from the box.
- **Leaf 0xD** (XSAVE/XCR0 state components & sizes) — AVX-512 absent ⇒ no ZMM/opmask state ⇒
  different XCR0-valid mask and `xsave`-area sizes; re-derive (and keep the `dyn:xcr0-xsavesize` /
  `dyn:osxsave` formula tokens — only the constants change).
- **Leaf 0xB/0x1F** (topology) — keep the synthetic single-thread topology; confirm encodings match
  the CFL leaf shape.
- **Leaf 0x15 / 0x16** (TSC/crystal/bus & core-freq) — confirm `crystal-hz = 25_000_000`,
  `bus-hz = 100_000_000`, and `tsc-hz` against the box; the 9900K base is 3.6 GHz (these feed the
  §5 timer model — change only if the box dump contradicts the frozen scalars).
- **Leaf 0x80000008 EAX** `0x0000302E` (46 phys/48 virt) → `0x00003027` (**39** phys/48 virt) — the
  `maxphyaddr-min` driver. Confirm 39 on the box.
- Any other leaf whose raw value differs between `cpuid-raw.txt` and the current `det-skx-v1` table.

**MSR tables (§3) — the microcode-fingerprint rows MUST be re-derived from the box, not carried:**
- **§3.9 `MSR_IA32_ARCH_CAPABILITIES` (0x10a)** — the current frozen `0x400000000D10E171` is a
  **Skylake-SP-microcode fingerprint**. The correct frozen value for the 9900K under microcode
  `0xf8` differs (different mitigation-enumeration bits). **Read it from the box** (`rdmsr 0x10a`),
  reconcile bit-by-bit with the row's rationale, and freeze the box value. Re-check the DOITM(12) /
  `IA32_UARCH_MISC_CTL` (0x1b01) pairing and the TSX_CTRL_MSR(7) bit against the 9900K.
- **§3.9 speculation class** (SPEC_CTRL 0x48, etc.) and any other row whose disposition or frozen
  value is justified by "host microcode/µarch" — re-validate against the box; CFL+µcode-0xf8 may
  enumerate a different set than SKX.
- **§3.10 `microcode` class** — the pinned revision narrative updates to `0xf8`.
- Rows justified purely by SDM architecture (not host µarch) carry over unchanged.

**TSX reclassification — present-but-aborting (class c) → physically absent (#UD, class b):**
- §4 instruction table: move **XBEGIN/XEND/XTEST/XABORT** out of the `(c) intercept / always-abort`
  row into a `(b) fault-absent / #UD` disposition — on the 9900K these `#UD` natively (RTM absent),
  no `IA32_TSX_CTRL` pin needed.
- §1.1 narrative, §1.2/§3.4 (`IA32_TSX_CTRL` 0x122) and §3.5 prose, and the §3.9 ARCH_CAP TSX_CTRL
  bit: rewrite the "deterministic always-abort on TSX-capable SKX" reasoning to "TSX physically
  absent ⇒ native `#UD`; CPUID.7.0:EBX[4,11]=0; no host pin required." Keep the **outcome** invariant
  (TSX is non-usable by the guest, deterministically) — only the *mechanism* changes. Note the
  `hostassert.rs` `rtm-disabled` assert **already** accepts "rtm physically absent" and passes; no
  code change should be needed there, but confirm it passes on the box.

## Recompute the §6 contract hash and arm the anti-drift gate

The `contract_hash` is **computed, never hand-written**. After the tables are final:
1. Run `vmm-core`'s canonical serializer to get the v3 hash:
   `cargo test -p vmm-core contract -- --nocapture` (the `contract_hash*` tests print it), or a tiny
   harness calling `vmm_core::contract::contract_hash()`.
2. Write the hex into **both** `docs/cpu-msr-contract.toml` `[contract] contract_hash = "<hex>"` and
   `docs/CPU-MSR-CONTRACT.md` §6.
3. **Un-ignore** `consonance/vmm-core/src/contract/mod.rs::contract_hash_matches_committed_registry`
   (currently `#[ignore]` pending the committed hash) so the registry-match gate is live and green.
   This is the only `vmm-core` source change expected; it does not touch contract *policy* code.

## Validate on the box — the acceptance bar

This is the whole point of the task, so it is a hard gate:
- `cargo test -p vmm-core --test live_m1_m2 host_assert_report -- --nocapture` **on the box** must
  now show **all** §1.1 assertions **PASS** (previously 3 failed). Paste the full report into
  `IMPLEMENTATION.md`. (Run via `ssh <det-box>` per `docs/BOX-PINNING.md` — CPU-pin the workload.)
- The `contract_hash_matches_committed_registry` gate is green (computed == committed).
- The MSR index-set partition still validates (pairwise-disjoint; total count unchanged unless a row
  legitimately moved class — call out any count change).

## Gates

Standard gates on `vmm-core` (the only crate touched), plus the doc/data consistency:
```sh
cargo build  -p vmm-core --all-features
cargo nextest run -p vmm-core --all-features        # incl. the un-ignored hash gate
cargo clippy -p vmm-core --all-features --all-targets -- -D warnings
cargo fmt    -p vmm-core -- --check
```
- The toml must parse and round-trip through `vmm-core`'s contract loader; md ↔ toml consistent.
- macOS builds/tests the pure-logic + hash path; the **host-assert PASS evidence is box-only** and
  goes in `IMPLEMENTATION.md`.

## Scope / isolation

- Touch `docs/CPU-MSR-CONTRACT.md`, `docs/cpu-msr-contract.toml`, `docs/fragments/cfl-baseline/`
  (new raw evidence), `consonance/vmm-core/src/contract/mod.rs` (un-ignore the one gate only), and
  `consonance/vmm-core/IMPLEMENTATION.md`. **Do not** touch contract *policy* logic, other crates, or
  the host-assert *logic* (data only).
- `IMPLEMENTATION.md`: list every changed constant with its box-evidence line, and the new hash.

## Deliverables

1. `det-cfl-v1` / version-3 contract in md + toml, every host-specific value derived from and
   cited to the box dump under `docs/fragments/cfl-baseline/`.
2. Recomputed §6 `contract_hash` committed in both files; the registry-match gate un-ignored & green.
3. Box evidence in `IMPLEMENTATION.md`: full §1.1 host-assert report showing **all PASS** on the 9900K.
4. All gates green; MSR partition still valid.

When opening the PR, summarize the change-set as a table (row → old → new → box-evidence) so the
review can audit it fast. Expect a rigorous review (this is the determinism anchor) including a
cross-model pass and per-constant spot-checks against the raw dump.
