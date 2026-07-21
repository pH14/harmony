# consonance/acceptance-suite/payloads/ — bare-metal Part-A payloads

Tiny Multiboot-v1 payloads with fully known, timing-independent serial output,
checked byte-for-byte against `consonance/acceptance-suite/golden/`. The boot/UART/exit/IDT pipeline
and the "add a payload" flow are documented in [`../README.md`](../README.md)
(Part A). This file documents the **C1 instruction-sweep** corpus (task 18) and
its coverage of the trapped instruction / MSR surface.

## C1 payloads (task 18)

One payload per trapped instruction / MSR class in `docs/cpu-msr-contract.toml`.
Each asserts only the **environment-independent shape** in the serial banner — so
the stock-QEMU Part-A gate is green on macOS and Linux — and reports the
trap-dependent, machine-checkable values via `common::report`, which emits them
over the dedicated **report channel** (`OUT 0x0CA2`, the corpus box-integration's
own port — distinct from the `0x0CA1` hypercall doorbell; see `report.rs` and
`docs/INTEGRATION.md` §1.1). On the box `vmm-core` captures the stream and digests
it (with the serial banner) into the O2 `observable_digest`; under stock QEMU
(no device at the port) the writes are discarded, so the Part-A serial gate stays
byte-identical. The manifest (`docs/corpus-manifest.toml`) registers each payload
for **O1 determinism** and **O3 seed-sensitivity**, plus **O2 conformance**
(`consonance/acceptance-suite/golden/<name>.digest`, box-captured) for the six payloads that reach a
clean PASS on vmm-core's current event loop — four payloads are O2-deferred until
vmm-core models V-time timers + IRQ injection (insn-hlt, irq-landing, pit-pic-stub,
the "LAPIC timer landing" hard core) and MONITOR/MWAIT (insn-mwait, which exits
DebugExit 1 on the event loop today); see
[`../../../docs/DETERMINISM-CORPUS.md`](../../../docs/DETERMINISM-CORPUS.md) §C1 and
`consonance/vmm-core/tests/box_corpus.rs`.

| Payload | Instruction(s) / MSRs | In-guest (QEMU) assertion | Reported for the box | O3 |
|---------|-----------------------|---------------------------|----------------------|----|
| `insn-rdtsc` | RDTSC, RDTSCP | TSC non-decreasing across 64 reads; RDTSCP ≥ prior | all readings (strict monotonic + Δ == 2×V-ns), TSC_AUX | pure |
| `insn-rng` | RDRAND, RDSEED | advertised ⇒ never faults, eventually CF=1 | collected values (== seeded contract PRNG stream) | rng-consuming |
| `insn-cpuid` | CPUID | every contract leaf/subleaf stable across reads | live regs + (matches,total) vs frozen model | pure |
| `insn-rdpmc` | RDPMC | faults and resumes (#GP box / #UD TCG) | per-selector #GP disposition | pure |
| `insn-hlt` | HLT | halts and is woken by the armed PIT deadline | pre-halt work markers (idle-skip) | pure |
| `insn-mwait` | MONITOR, MWAIT, PAUSE | PAUSE no-op; MONITOR/MWAIT execute or #UD-and-resume (never hang) | #UD disposition | pure |
| `msr-allowed` | RDMSR/WRMSR allowed set | **every** allow-stateful MSR round-trips (write→read→restore) | (index, written) per allow-stateful MSR + allow-fixed reads vs contract values | pure |
| `msr-denied` | RDMSR/WRMSR unknown | probes the default-deny surface without panicking | per-index #GP disposition (== default-deny) | pure |
| `irq-landing` | LAPIC timer | each armed deadline delivers exactly one IRQ | armed deadlines (box pins retired-count, sweep ±1 of skid_margin=128) | pure |
| `irq-landing-rng` | LAPIC timer + RDRAND | each *seed-derived* deadline delivers exactly one IRQ | seed-derived deadlines (preemption instant is a pure function of the seed; task-47 gate-2 seed-dependence) | rng-consuming |
| `pit-pic-stub` | PIT, PIC | PIC init + N deterministic PIT ticks; port 0x61 read | refresh bit + tick count (V-time cadence) | pure |

## Trap-surface coverage checklist (gate 1)

Every trapped instruction / MSR class in `docs/cpu-msr-contract.toml` maps to a
payload, or is named here as a deliberate omission with its reason. The failure
mode to avoid is a *silent* gap.

### `[insn]` rows

| Disposition | Mnemonics | Covered by |
|-------------|-----------|------------|
| intercept → V-time | RDTSC, RDTSCP | `insn-rdtsc` |
| intercept → seeded PRNG | RDRAND, RDSEED | `insn-rng` |
| intercept → frozen model | CPUID | `insn-cpuid` |
| intercept → #GP | RDPMC | `insn-rdpmc` |
| intercept → idle-skip | HLT | `insn-hlt` |
| intercept → #UD | MONITOR, MWAIT | `insn-mwait` (PAUSE permitted, also here) |

**Deliberately omitted `[insn]` rows (with reasons):**

- **VMCALL** (`hypercall-dispatch`) — the hypercall doorbell (#44), not a
  guest-observable trap to pin here. The corpus value-report channel chose its
  **own** port (`OUT 0x0CA2`, distinct from the `0x0CA1` doorbell — see
  `report.rs` / `docs/INTEGRATION.md` §1.1), so a reported value is never confused
  with a doorbell ring.
- **XSETBV** (`xcr0-menu{1,3,7}-else-gp`, intercept) — the XCR0/XSAVE policy is
  reported via `insn-cpuid` leaf 0xD; a dedicated payload probing XCR0 ∈ {1,3,7}
  allowed vs #GP otherwise is a reasonable future C1 item, out of this task's
  ten-row scope.
- **The uniform-#UD rows** — VMX family (VMCLEAR/VMLAUNCH/VMPTRLD/VMPTRST/VMREAD/
  VMRESUME/VMWRITE/VMXOFF/VMXON/INVEPT/INVVPID), RDPKRU/WRPKRU (cr4-pinned),
  HRESET/PCONFIG/RDPID/SERIALIZE/SHA/TPAUSE/UMONITOR/UMWAIT (host-absent on
  det-cfl-v1), TSX (XABORT/XBEGIN/XEND/XTEST), XGETBV1/XRSTORS/XSAVEC/XSAVEOPT/
  XSAVES (`scope`/native-uninterceptable). All resolve to the **same** #UD
  disposition that `insn-rdpmc`/`insn-mwait` already exercise the fault-catch
  mechanism for; a single `insn-ud-sweep` future item could enumerate them, but
  the marginal signal over one #UD probe is low.
- **permit-native rows** — FXSAVE/FXRSTOR, XSAVE/XRSTOR, XGETBV0 (`arch`): not
  trapped (permitted native, determinism is architectural); the `compute`
  payload already drives FP/arch state. Out of the *trap*-sweep scope.

### `[msr]` rows

| Disposition | Covered by |
|-------------|------------|
| allow-fixed (read returns frozen value) | `msr-allowed` (read + report; box pins value) |
| allow-stateful (read/write round-trip) | `msr-allowed` — **exhaustive**: every allow-stateful index (task 31) |
| default-deny (unknown index → #GP) | `msr-denied` (spread of off-contract indices) |

The allow-stateful round-trip is **complete, not sampled** (task 31): `msr-allowed`
sweeps `contract_data::MSR_ALLOWED_STATEFUL` (generated from the TOML), and the
`contract-data` test `sweep_set_equals_contract_allow_stateful` re-parses
`cpu-msr-contract.toml` and asserts the swept set *equals* the contract's
allow-stateful set — so adding an allow-stateful row to the contract without
extending the sweep fails loudly. Each index is written a contract-legal value
(canonical address, valid memory-type encoding, reserved bits clear; `EFER` is a
read-modify-write toggle of `SCE` that preserves `LME`/`LMA`), tabulated and
legality-checked in `contract_data::roundtrip_value`. This covers the MSRs Linux
configures on the boot path (`EFER`, `LSTAR`, `SYSENTER_*`, the MTRR block,
`CR_PAT`, the `STAR`/`*_BASE` family), de-risking the kernel boot (task 30).

**Deliberately omitted / partial `[msr]` cases (with reasons):**

- **emulate-vtime MSRs** (IA32_TSC 0x10, IA32_TSC_ADJUST 0x3b) — the TSC value
  semantics are exercised by `insn-rdtsc` (RDTSC reads the same V-time source);
  a direct RDMSR(0x10) monotonic check is a minor possible addition. Exact value
  is box-only.
- **explicit deny-gp MSRs** (named rows: PMU, x2APIC 0x800–0x8FF, speculation,
  power/thermal, Intel-PT, …) — share the #GP disposition with default-deny;
  `msr-denied` proves the #GP mechanism on the *unknown* surface. A
  named-deny-gp sweep would re-test an identical disposition, so it is omitted
  (the indices would also fail `msr-denied`'s "off-contract" invariant by
  construction).
- **deny-ignore-write MSRs** (APICBASE, UCODE_REV, MISC_ENABLE writes) — the
  write-is-silently-ignored behavior is box-only (no guest-observable effect
  under QEMU); their *reads* are covered as allow-fixed by `msr-allowed`.

### Timer / device surface

| Device | Covered by |
|--------|------------|
| LAPIC timer (mmio 0xfee00000) | `irq-landing` |
| PIT ch0 + 8259 PIC + port 0x61 | `pit-pic-stub` |

PIT ch1/ch2, RTC/CMOS, HPET (deny-gp), ACPI-PM (deny-gp) and the full xAPIC MMIO
register file are not individually swept here — boot stubs touch only ch0/PIC and
the LAPIC timer path; the wider device-verb conformance is task 21's scope
(docs/DETERMINISM-CORPUS.md backlog).
