<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# AA-6 contract enforcement — ID-register freeze + vGIC round-trip, N1, 2026-07-20

Two AA-6 contract mechanisms proven on the determinism host `6.18.35-aa4guard` (build-id
`ac576f87`, core 60). Both run on their own disposable VM/vCPU(/vGIC) via new additive
`arm-spike` subcommands (`id-freeze`, `vgic-roundtrip`) — no change to the run loop or W^X.

## AA-6(a) ID-register-freeze enforcement — PASS (`id-freeze.json`)

`all_enforced=true`: eight `ID_AA64*` registers each accepted a **below-host** feature value
via `KVM_SET_ONE_REG`, and the **read-back holds it** — the value a guest's EL1 `mrs`
observes, since KVM emulates ID reads from the vCPU's stored register. Notably
`ID_AA64PFR1_EL1` freezes here (host nibble `0x2`→`0x1`) — the register that was **frozen on
stock 6.8**; the AA-0 patched re-probe predicted this, and it is now demonstrated on the
enforcement path. `pmu_denied_without_feature=true`: a vCPU created without
`KVM_ARM_VCPU_PMU_V3` reads `ID_AA64DFR0_EL1.PMUVer` as 0 while the sanitised host value is 4
— the guest is denied its own PMU (the contract). This is the enforcement-mechanism truth
table AA-6(a) calls for: every reduced row installs and is guest-visibly frozen.

## AA-6(b) vGIC save/restore round-trip — PASS (`vgic-roundtrip.json`)

Machine A enables + sets pending on **PPI 20** (AA-5's dedicated clockevent line); its 15
redistributor private-IRQ registers are saved via `KVM_DEV_ARM_VGIC_GRP_REDIST_REGS`. A fresh
machine B differs (`negative_control_differs=true` — `ISENABLER0 0x0000ffff`, `ISPENDR0 0x0`),
the save is restored into it, and the re-read is **byte-identical** to A
(`roundtrip_identical=true` — `ISENABLER0 0x0010ffff`, `ISPENDR0 0x00100000`, bit 20
transferred exactly). vGIC injection state round-trips faithfully — the AA-6(b) decision
input recorded as measured, not argued.

## Scope note
These are the **contract-enforcement** and **device-round-trip** halves of AA-6. The full
**determinism-under-injection matrix including the AA-5 Linux guest** remains — it needs a
Linux-guest injection-record path that extends the frozen run/boot code, held pending the
PR #135 review (see `../../BOX-WINDOW-2026-07-20.md`).
