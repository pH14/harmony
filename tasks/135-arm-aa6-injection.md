# Task 135 — ARM AA-6: contract enforcement + injection + the mini determinism gate

**Spike task. Binding spec: `docs/ARM-ALTRA.md` §AA-6 (line ~582).** Beads: **hm-zx3z** (scope, P2), **hm-l1wy** (proof-completeness F8/F9/F10), parent **hm-idb**. This is the **last gate** for the full AA-0..AA-6 ARM GO re-cert (AA-3 on-silicon re-cert is the only other open piece). PR #135 (AA-4/AA-5(c) apparatus) is MERGED — build on that merged apparatus.

Spike discipline (from `docs/ARM-ALTRA.md`): **No Beads for planning** — durable state lives in `spikes/arm-altra/` evidence dirs + machine-readable manifests + this doc's dispositions. Exclusive box lock; serialize every box run; smoke-fire-once before any ≥10⁴-sample or ≥30-min run; commit+push evidence promptly (the box is wipe-prone). One recorded disposition per stage (GO / PROVISIONAL GO / REDESIGN / NO-GO).

## The AA-6 deliverables (spec §AA-6)

1. **(a) `ID_AA64*` freeze + enforcement truth table.** Install a shrunk synthetic ID-register model through KVM's writable-ID-register surface; verify the guest sees frozen values (incl. feature bits *below* host capability); enumerate `HCR_EL2`/`MDCR_EL2` trap groups against the §5 contract-row skeleton — PMU sysregs denied (observe: guest PMU reads/writes **fault** — a real access-fault proof, not inferred from the ID nibble: **hm-l1wy F10**), counter rows per AA-5(b). Deliverable: the **enforcement-mechanism truth table** — every planned contract row → a demonstrated trap/freeze, or recorded undeniable-on-N1 with a disposition. Preserve the id-freeze **tri-state** (distinguish "no reducible field" from "reducible-but-clamped" — **hm-l1wy F9**).
2. **(b) vGIC decision input.** In-kernel vGICv3 state save→restore→save round-trip (`KVM_DEV_ARM_VGIC_GRP_*`): bit-identical? Extend beyond the 15 redist SGI/PPI regs to distributor / CPU-interface (ICC_PMR/IGRPEN) / external-line state (**hm-l1wy F8**). Injection through the vGIC at a landed `Moment`: reproducible? Record the userspace-GIC-vs-in-kernel-vGIC decision verdict — measured, not argued.
3. **(c) The mini determinism gate.** Same seed twice → **bit-identical state digest**, on the spike harness over the payload matrix **plus the AA-5 Linux guest**, with events injected at seeded-random `Moment`s — the whole stack (work clock, exact landing, LSE-only contract, paravirt time, frozen IDs) exercised together. **≥1,000 same-seed mini-gate repetitions bit-identical**, every attempted sample accounted for, floors machine-checked against retained records.

## The run-core change — non-additive, careful (hm-zx3z)

The injection hook needs changes to the default `linux_boot`/`run` path + the bare-payload `run_sample` record machinery, which **risks the determinism core**. Discipline:
- **Config-gate the hook.** Prove the default deterministic path is **byte-identical with injection OFF** — a negative control that a fresh build with the hook present but disabled reproduces the pre-hook `state_hash` bit-for-bit. This is the "flag-the-run-loop" rule; the hook must be non-additive to the OFF path.
- Capture vGIC + ID-freeze **under injection**.
- This is determinism-core code → it gets a **full tribunal review** on the PR. Open the PR early (after the hook + OFF-path negative control are green portably) so the seam gets reviewed before the box matrix spend.

## Acceptance (spec §AA-6)

Truth table complete; vGIC round-trip verdict recorded; **≥1,000 same-seed mini-gate reps bit-identical**, every sample accounted for, floors machine-checked. Record the AA-6 disposition (GO / PROVISIONAL GO / REDESIGN / NO-GO with the gap named) in `docs/ARM-ALTRA.md`. **Stop** (REDESIGN or NO-GO) if: an unfreezable guest-visible register reaches state, or vGIC state can't round-trip *and* no userspace-model shape exists.

## Environment

Box: `ssh harmony-arm` (Ampere Altra / Neoverse N1, `/dev/kvm`). It is **up and on stock 6.8** with the patched kernels **already installed** from the 2026-07-21 window (`/boot/vmlinuz-6.18.35-aa3preempt`, `aa4guard`). AA-6 injection needs **aa3preempt** (the patched force-exit). Boot into it with **grub-reboot / boot-once** (NOT grub-set-default — stock stays the default so a hang self-recovers; the foreman can always reboot). Pin every box run with `taskset` per `docs/BOX-PINNING.md`, SMT sibling idle. Bundle-transfer code (git push to the box is classifier-blocked): `git bundle` + scp + fetch. **Commit+push evidence promptly** — the box was account-wiped once (2026-07-20); nothing uncommitted crosses a reboot. Release the exclusive lock + revert to stock when done. Guest artifacts/ROM per the merged #135 setup.
