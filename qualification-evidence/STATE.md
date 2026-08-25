# Resume state

Worktree: `/Users/phemberger/workspace/harmony/.claude/worktrees/agent-a98a1db9c86d06f8c`
Branch: `task/cpu-qualification-suite`. Box: `ssh -i ~/.ssh/id_ed25519 root@62.210.145.82`.
Scratchpad: `/private/tmp/claude-501/-Users-phemberger-workspace-harmony/a54e77d2-0ede-4a28-96d4-7ccc43759491/scratchpad`.

## Done

- Items 1, 2, 3, 4, 5 complete. Pack `docs/chips/det-zen3-v1.toml` sealed
  `e233f43ad4a6272a9a9cbebd9bc059b7407fd5e8018859680195e2ab68374be2`.
- Stage 1 campaign E passes: `report --evidence-dir /root/qual-evidence/stage1-E`
  exits 0, 36 of 36 checks. Independently recomputed outside the suite.
- Item 6 patch blocker resolved: the shipped `amd-svm.patch` is a malformed diff, not
  context drift. Re-anchored by hand as kernel commit `52248ae11` in
  `/root/kbuild-618/linux-6.18.35`; the re-anchored diff is saved at
  `qualification-evidence/box/stage2/amd-svm-reanchored.patch`. One of the two
  permitted patch attempts is used.
- Item 7 stock half complete, written up in `qualification-evidence/stage2-stock-findings.md`.

## Running

- 6.18.35 kernel build on the box, log `/root/qual-evidence/stage2/kbuild2.log`.
  Alive while `pgrep -cx make` is non-zero. Done when `/root/kbuild-618/*.deb` exists.
  Never match a build process by a `-f` pattern; it matches this session's own argv.

## Next

1. Campaign running since 04:26:09Z: `/root/qual-evidence/stage2/campaign/core{1,3,5,7,9,11,13,15}.json`,
   8 cores x 62500 targets, `--replay` (so 500000 targets are 1000000 armed deadlines),
   margin 16192 which is the pack's sealed value. Expected finish about 07:35Z.
   Live view: `/root/qual-evidence/stage2/campaign/progress.log`, one line a minute with
   a per-shard arm count and the failing count. PIDs in `.../campaign/pids`; alive check
   is `kill -0` per PID, never a `-f` pattern.
   Two recorded failures so far: core 9 index 15257 (replay digest mismatch) and core 15
   index 28164 (overshoot, skid 37595). Neither is a reason to widen anything.
2. Immediately after: `bash /root/post-campaign.sh <N>` where N is the supplement's
   targets per core, sized so the count of landings through KVM_EXIT_PREEMPT passes
   1000000 (about 83.8 percent of arms take that exit, since targets are uniform on
   [1,100000] and the margin is 16192). It runs the supplement on seven cores and the
   seed-109 reproduction of the core-9 mismatch on the eighth, using `ae3-diag`, a copy
   of the harness that also records the replay arm's own landing.
3. Still on the patched kernel: `bash /root/close-a.sh` re-runs the enforcement demo on
   6.18.35, the kernel the AMD draft contract column names, and a 1000-repetition
   whole-stack check as supporting evidence.
4. Item 8: rsync, `bash /root/kclose2.sh`, reboot, poll ssh, `bash /root/posture.sh`,
   ship the final pack, `bash /root/stage0-recheck.sh`, rsync, final commit, report.

## Standing facts

- The work-clock event is locked at `0x5100d1`. Never substitute another.
- `ship.sh` replaces the box tree, so anything written into `/root/harmony` by hand is
  lost on the next ship.
- `check` requires `perf_event_max_sample_rate = 100000000` and
  `perf_cpu_time_max_percent = 0`. The kernel refuses a write to the rate while the
  percent is 0 or 100, so set percent 50, then the rate, then percent 0.
