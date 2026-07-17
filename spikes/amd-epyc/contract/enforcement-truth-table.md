<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# AE-4 — AuthenticAMD contract vendor column: enforcement truth table

`docs/AMD-EPYC.md` §4 / Definition-of-done #5. The guest-visible CPU surface frozen as a
**vendor column on the one contract, never a fork** (`docs/GLOSSARY.md`
never-fork-the-one-reproducer). This file is the **enforcement-mechanism truth table**:
each contract row → the SVM trap/freeze that enforces it (or "undeniable on this silicon"
with a disposition). It does **not** fork the contract — the dispositions live in the
existing draft:

- **Contract column (do not fork):** `docs/cpu-msr-contract-amd-draft.toml` — the
  materialized AuthenticAMD CPUID leaves (0, `0x8000_0000`–`0x8000_0008`) and the AMD MSR
  set (`0xC000_00xx`/`0xC001_00xx`), `cpuid-baseline = "det-zenN-v1"`, every cell marked
  `verified = "on-silicon-pending-AE4"`. AE-4 ratifies those cells; the vendor-axis
  restructure of the loader is production pre-build work (`hm-0nf`), out of spike scope.
- **This file** pins `N = 2` (the AE-0 Zen generation) and maps each row to its SVM
  enforcement backend + the on-silicon verification status.

## AE-0 silicon facts that fix the per-generation contract rows

From `results/ae-0/capability-truth-table.json`:

| fact | value (Ryzen 3600, Zen 2) | contract consequence |
|---|---|---|
| vendor string | `AuthenticAMD` | CPUID leaf 0 EBX/ECX/EDX frozen to `Auth`/`enti`/`cAMD` |
| PMU model | **legacy per-counter** (`PerfMonV2 = false`, leaf `0x8000_0022` absent) | the PMU column is the legacy `PERF_CTL`/`PERF_CTR` (`0xC001_020x`) set; the PerfMonV2 global MSRs (`0xC000_0300`–`3`, `applies-when = zen4+`) are **N/A on this part** — a recorded per-generation deviation, not a gap |
| AVIC | present in silicon, **disabled** (`kvm_amd avic=0`) | interrupt fabric stays in userspace (determinism posture); recorded standing condition |
| RDTSCP / RDRAND / RDSEED / invariant-TSC | all present | intercept-controllable rows apply |
| SVM MSR-permission-bitmap, `#DB`/DR intercepts, NRIP-save, DecodeAssists | present | the enforcement backends below are all available |

The frozen baseline name is therefore **`det-zen2-v1`** (the draft's `det-zenN-v1` with
N pinned to 2).

## Enforcement truth table (each contract row → its SVM mechanism)

| contract row | disposition (from draft) | SVM enforcement backend | on-silicon status |
|---|---|---|---|
| CPUID leaf 0 vendor string | `allow-fixed(AuthenticAMD)` | VMCB **CPUID intercept** → `KVM_SET_CPUID2` frozen model | verify-on-silicon (KVM harness) |
| CPUID `0x8000_0001` feature bits | `allow-fixed` (frozen ≤ host) | VMCB CPUID intercept; **below-host bits** cleared in the frozen model | verify-on-silicon — incl. a feature bit *below* host capability |
| CPUID `0x8000_000A` (SVM features) | `allow-fixed`/`deny` | CPUID intercept (guest is not offered nested SVM in the bare-metal cell) | verify-on-silicon |
| CPUID `0x8000_0022` (PerfMonV2) | **absent on Zen 2** | n/a — leaf not present; frozen model omits it | **confirmed absent (AE-0)** |
| syscall/segment MSRs `0xC000_0080`–`0103` | `allow-stateful` | VMCB **MSR-permission-bitmap** pass-through; captured in `vm_state` | verify-on-silicon |
| `HWCR 0xC001_0015`, `VM_HSAVE_PA 0xC001_0117`, `LS_CFG 0xC001_1020`, `DE_CFG` | `deny-gp` | MSR-permission-bitmap trap → `#GP` injected | verify-on-silicon |
| legacy PMU `PERF_CTL/CTR 0xC001_020x`, `MSR_K7_* 0xC001_000x` | `deny-gp` | MSR-permission-bitmap trap; `RDPMC` → VMCB RDPMC intercept → `#GP` | verify-on-silicon |
| PerfMonV2 global `0xC000_0300`–`3` | `deny-gp` (`applies-when = zen4+`) | **N/A on Zen 2** (MSRs absent) — row inert on this part | **N/A (AE-0)** |
| `RDTSC`/`RDTSCP` | `emulate-vtime` | VMCB RDTSC/RDTSCP intercept → V-time map | verify-on-silicon |
| `RDRAND`/`RDSEED` | `deny`/emulate | VMCB RDRAND/RDSEED intercept → vmm | verify-on-silicon |
| unlisted leaves/MSRs | `deny-gp` (default) | MSR-permission-bitmap default-deny + CPUID intercept | verify-on-silicon |

## Disposition

**PROVISIONAL — skeleton complete, enforcement demo is the AE-4 box step.** The vendor
column already exists as a draft (`docs/cpu-msr-contract-amd-draft.toml`); AE-0 fixed its
two per-generation facts (legacy PMU present, PerfMonV2 absent — `det-zen2-v1`), and the
enforcement backend for every row is available in the SVM feature surface confirmed at
AE-0 (CPUID intercept, MSR-permission-bitmap, RDTSC/RDPMC/RDRAND/RDSEED intercepts). The
remaining step is the on-silicon demonstration via the KVM harness (guest sees frozen
values incl. below-host bits; unlisted rows default-deny; PMU/RDTSC/RDRAND enforcement
reaches the vmm) — a `KVM_SET_CPUID2` + `KVM_X86_SET_MSR_FILTER` boot with a probe guest,
which shares the KVM-harness apparatus with AE-1(b)/AE-2. No row is undeniable on this
silicon (no NO-GO); the PerfMonV2 rows are inert on Zen 2 and re-confirm on a Zen 4 EPYC.
