# Program state

Worktree: `/Users/phemberger/workspace/harmony/.claude/worktrees/agent-a98a1db9c86d06f8c`
Branch: `task/cpu-qualification-suite`
Box: `ssh -i ~/.ssh/id_ed25519 root@62.210.145.82`, evidence at `/root/qual-evidence`
Helpers (scratchpad): `ship.sh` (git archive → scp → rebuild), `pull.sh` (rsync evidence
into `qualification-evidence/box/`), `posture.sh` (also on the box at `/root/posture.sh`).
`ship.sh` replaces the box tree, so any diagnostic `examples/*.rs` must be rewritten after it.

## Done

- **Item 1** — `docs/chips/det-zen3-v1.toml` written, sealed, registered in `pack.rs`,
  public-API snapshot updated, gates green. Commit `a6c6e6d2`. Skid margin still the
  provisional 16384; item 5 replaces it with 2 × this chip's observed maximum.
- **Item 2** — posture applied and re-applied after every reboot: `spec_store_bypass_disable=on`
  and `isolcpus=3 nohz_full=3 rcu_nocbs=3` on the kernel command line; `LS_CFG` (0xC0011020)
  = 0x44480000000000 on every thread; NMI watchdog 0; governor performance; SMT off;
  AVIC off; md1 resync frozen; `perf_cpu_time_max_percent` 0.
- **Item 3** — stage 0 exits 0 twice, the second after a reboot, 50 identical rows.
  Records: `box/stage0-run1/`, `box/stage0-run2/`.

## Suite defects found and fixed (one commit each)

1. `49e86362` — the AMD lock probe had no event and the wrong polarity versus rr.
2. `bd434471` — one expectation was compared against every place a condition was read.
3. `b4be3c57` — an interference condition the host cannot produce aborted all of stage 1.
4. `4747d2e0` — `report` took its floors from the first plan in the directory, which for a
   stage-1 run is stage 0's all-zero one, and refused the second plan and terminal record.
5. `a4669e10` — a guest-half failure discarded every host-side stage-1 record.
6. `de9f5faf` — the exactness oracles counted taken branches, not conditional branches:
   `branch_dense` claimed 9 per iteration and `call_ret` 3, against a clock that counts 1.
   The same number drives the overflow arming, so two payloads never overflowed at all.
7. `a6f70a93` — the guest window's vCPU had no CPUID model, so KVM refused paravirtual
   MSRs its own index list names.
8. `01c2aee6` — the overflow summary is written once per armed period and cross-checked
   as if written once per payload.
9. `7c7aa587` — the interrupt-free window floor was the repetition count itself.
10. `69371af6` — the fixpoint restored the host-wide MSR index list rather than the MSRs
    the window's vCPU owns.
11. `7f8a6f88` — three free-running time bases (TSC, HV_X64_MSR_VP_RUNTIME,
    HV_X64_MSR_TIME_REF_COUNT) can never be a fixpoint by value; they are now held to
    advancing and everything else to standing still.

## Running

Stage 1 campaign E: `/root/qual-evidence/stage1-E`, PID 17416, launched 03:32Z, about nine
minutes. The final qualifying run. Nothing else runs on the box.

## Campaign D, the run before it

35 of 36 checks passed. Every arm accounted: 1,250,000 overflows delivered exactly once,
nothing throttled, dropped, lost, duplicated, premature or over margin. Skid min 34, max
7991. All 19 exactness checks, all 5 interference checks, all 5 summary cross-checks, the
plan, the terminal codes and the 50 host rows passed. The single failure was `fixpoint[0]`,
on `HV_X64_MSR_TIME_REF_COUNT`, which the kernel source shows is read-only by design;
campaign E is the same measurement with that register classified.

## Findings for the report

- `HV_X64_MSR_TIME_REF_COUNT` (0x40000020): read-only synthetic hypervisor state. Excluded
  from the must-restore set on the kernel's own comment. Not a silicon finding, not a KVM
  defect. Evidence: `hyperv-time-ref-count-classification.md` and `hyperv-6.12.95.c`.
- `MSR_KVM_ASYNC_PF_INT` (0x4b564d06): the one MSR of the host-wide index list this vCPU
  does not own, gated on an in-kernel local APIC the measurement window does not create.
- The kernel's sampling ceiling is now a standing host condition in the pack and in
  `check`, at both knobs, so a later run cannot measure under the stock ceiling silently.
- `smt-sibling` interference is not planned for this baseline: it requires the sibling
  thread the baseline turns off.

## Next

- Item 6: `bash /root/kbuild.sh` on the box, then
  `/root/spike/spikes/amd-epyc/host/stage-6.18-boot.sh install` and the GRUB one-shot.
- Item 7: `bash /root/stage2-stock.sh` for the half needing no patched kernel;
  `ae3-forceexit --event 0x5100d1` needs the patched kernel.
- Item 8: stock boot, stage 0 re-pass, evidence synced, final pack commit.

## Known permanent gaps

- `smt-sibling` interference, as above.
- `count_offsets` and `single_step.work_per_step` stay absent: both need a stage 2 the
  suite does not have, and the goal forbids implementing it.

## Resume commands

```sh
cd /Users/phemberger/workspace/harmony/.claude/worktrees/agent-a98a1db9c86d06f8c
ssh -i ~/.ssh/id_ed25519 root@62.210.145.82 'ps -eo pid,etime,comm | grep cpu-qualif'
/private/tmp/claude-501/-Users-phemberger-workspace-harmony/a54e77d2-0ede-4a28-96d4-7ccc43759491/scratchpad/pull.sh
```
