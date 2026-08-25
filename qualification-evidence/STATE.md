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

Stage 1 campaign B: `/root/qual-evidence/stage1-B`, PID 8655, launched 02:55Z, pinned to
cpu3, console at `/root/qual-evidence/stage1-B/console.txt`. Nothing else runs on the box.
Campaign A2's verdict, for comparison: skid n=1244458 min=26 max=8034, zero mismatches,
zero lost, zero duplicated, zero premature, zero over margin.

## Next

- Item 5: seal the pack with the observed skid maximum and margin = 2 × it, from campaign
  B; then campaign C is the qualifying run judged against the sealed margin.
- Item 6: patched kernel 6.18.35. `apt-get install bc bison flex dwarves libssl-dev
  libelf-dev debhelper` is still outstanding; patches staged at `/root/kbuild-618/patches/`;
  build with `/root/spike/spikes/amd-epyc/host/build-6.18-kernel.sh`. Do not build while a
  campaign is measuring: the box must be quiet for the clean windows.
- Item 7: stage-2 measurements through the spike C harnesses at event 0x5100d1.
- Item 8: stock boot, stage 0 re-pass, evidence synced, final pack commit.

## Known permanent gap

`smt-sibling` interference cannot be measured: the baseline requires SMT off, which removes
the sibling. `report` therefore cannot exit 0 for stage 1 on this posture. This is recorded,
not worked around.

## Resume commands

```sh
cd /Users/phemberger/workspace/harmony/.claude/worktrees/agent-a98a1db9c86d06f8c
ssh -i ~/.ssh/id_ed25519 root@62.210.145.82 'ps -eo pid,etime,comm | grep cpu-qualif'
/private/tmp/claude-501/-Users-phemberger-workspace-harmony/a54e77d2-0ede-4a28-96d4-7ccc43759491/scratchpad/pull.sh
```
