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

1. Campaign running: `/root/qual-evidence/stage2/campaign/core{1,3,5,7,9,11,13,15}.json`,
   8 cores x 62500 targets, `--replay`, margin 16192, launched 04:26Z, projected 3.3h.
   PIDs in `/root/qual-evidence/stage2/campaign/pids`; alive check is `kill -0` per PID,
   never a `-f` pattern. Tally with `python3 /root/tally.py
   /root/qual-evidence/stage2/campaign`.
2. Still on the patched kernel, re-run the enforcement demo there: the AMD draft
   contract column names `kernel-tag = "v6.18.35"`, and the demo so far ran on stock
   6.12.95. `cd /root/spike/spikes/amd-epyc/harness && taskset -c 3 ./ae4-freeze` and
   `./ae4-msr`. Optionally `./ae5-gate` as supporting whole-stack evidence (stage 3 is
   out of scope, so label it as such).
3. Item 8: rsync, `bash /root/kclose2.sh` (restores the pristine command line and pins
   the stock entry), reboot, poll ssh, `bash /root/posture.sh`, ship the final pack
   (`ship.sh`, which replaces the box tree and rebuilds), `bash /root/stage0-recheck.sh`,
   rsync, final commit, report.

## Standing facts

- The work-clock event is locked at `0x5100d1`. Never substitute another.
- `ship.sh` replaces the box tree, so anything written into `/root/harmony` by hand is
  lost on the next ship.
- `check` requires `perf_event_max_sample_rate = 100000000` and
  `perf_cpu_time_max_percent = 0`. The kernel refuses a write to the rate while the
  percent is 0 or 100, so set percent 50, then the rate, then percent 0.
