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

1. `bash /root/kstage.sh` (install the .deb, arm the one-shot entry, trim
   `GRUB_CMDLINE_LINUX_DEFAULT` to `panic=30`).
2. Rsync `/root/qual-evidence/` into `qualification-evidence/box/`, then
   `bash /root/spike/spikes/amd-epyc/host/stage-6.18-boot.sh reboot`. Poll ssh up to
   10 minutes. Then `... verify`, then `bash /root/posture.sh` (every volatile setting
   is lost across a reboot).
3. `bash /root/stage2-patched.sh` — smoke (mechanism attestation, exit reason 42) then
   a 200-target calibration. Read the skid distribution with
   `python3 /root/skidstat.py /root/qual-evidence/stage2/ae3-calibration.json`, pick
   the tightest margin above the observed guest skid, project the landing rate, then
   run the volume campaign for at least 1e6 landings
   (`--arms 500000 --replay` counts as 1e6). Overshoot is a recorded failure.
4. Item 8: ship the final pack, `bash /root/kclose.sh` (restore stock), reboot,
   `bash /root/posture.sh`, `bash /root/stage0-recheck.sh`, rsync, final commit.

## Standing facts

- The work-clock event is locked at `0x5100d1`. Never substitute another.
- `ship.sh` replaces the box tree, so anything written into `/root/harmony` by hand is
  lost on the next ship.
- `check` requires `perf_event_max_sample_rate = 100000000` and
  `perf_cpu_time_max_percent = 0`. The kernel refuses a write to the rate while the
  percent is 0 or 100, so set percent 50, then the rate, then percent 0.
