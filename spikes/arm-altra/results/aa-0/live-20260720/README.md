<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# AA-0 capability truth-table — re-capture on the re-provisioned N1, 2026-07-20

The 2026-07-17 AA-0 GO was on the box that was later wiped; this re-establishes the
capability truth-table and reboot-determinism on the re-provisioned box (RL300 Gen11,
**BIOS 1.70** — `box-config.json` here carries this box's live firmware, not the old 1.74).

## Reboot-determinism gate — PASS

`capture-A/B/C/truth-table.json`, each captured on **stock `6.8.0-134`** across three
separate reboots (`perf_event_paranoid=-1` re-applied each boot), are **byte-identical**:

```
sha256 a6cec76bbdf300426678214a407d5bd1dc6f707dd41ede3992d6f47bfc769cdb  (A == B == C)
```

15 rows, every deviation confirmed or ruled. `writable-id-registers` is RULED on stock
(ID_AA64PFR1_EL1 frozen, per the committed 2026-07-17 ruling).

## Patched-host re-probe — `patched-aa4guard/`

A capture on `6.18.35-aa4guard` (build-id `ac576f87`) resolves the pending item from the
2026-07-17 ruling:

- **`writable-id-registers` = PRESENT on the patched host.** The stock-6.8 PFR1 freeze does
  **not** recur on 6.18.35, so AA-6(a)'s whole-surface synthetic ID-register freeze can
  install via KVM's writable-ID surface on the determinism host. The row is now confirmed,
  not a pending re-probe.
- `kvm-cap-arm-deterministic-intercepts` and `kvm-cap-arm-stage2-exec-guard` are **present**
  on the patched host (by construction of patches 0001/0002) — ruled favourable deviations
  (`patched-aa4guard/rulings.json`). Absent on stock, as expected.

Every existential row (KVM+VHE, raw 0x21 pinned counting, BR_RETIRED PMCEID1-implemented,
overflow delivery, guest-debug, vGICv3 creatable, ECV absent, LSE present) is confirmed on
both kernels; identity (MIDR 0x413FD0C1, 80 cores) is stable.
