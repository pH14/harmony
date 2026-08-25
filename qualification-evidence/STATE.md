# Resume state

Worktree: `/Users/phemberger/workspace/harmony/.claude/worktrees/agent-a98a1db9c86d06f8c`
Branch: `task/cpu-qualification-suite`. Box: `ssh -i ~/.ssh/id_ed25519 root@62.210.145.82`.
Scratchpad: `/private/tmp/claude-501/-Users-phemberger-workspace-harmony/a54e77d2-0ede-4a28-96d4-7ccc43759491/scratchpad`.

Never identify a box process by a `pkill -f` or `pgrep -f` pattern: it matches this
session's own argv. Use PIDs from the run's `pids` file and `kill -0`.

## Done

- Items 1, 2, 3, 4, 5 complete. Pack `docs/chips/det-zen3-v1.toml` sealed
  `f375e8a12cb35ca4aea4ca2210b5c0ac6c43ba6f88a3da09a079cc36f41a6139` (commit 532f51b2),
  carrying both skid distributions labelled and the measured overshoot handling.
- Stage 1 campaign E passes: `report --evidence-dir /root/qual-evidence/stage1-E`
  exits 0, 36 of 36 checks. Independently recomputed outside the suite.
- Item 6 complete: kernel 6.18.35 built with the re-anchored SVM patch (kernel commit
  `52248ae11` in `/root/kbuild-618/linux-6.18.35`, diff saved at
  `qualification-evidence/box/stage2/amd-svm-reanchored.patch`), staged through the
  GRUB one-shot after a QEMU pre-flight, and booted. One of the two permitted patch
  attempts was used.
- Item 7 stock half complete: `qualification-evidence/stage2-stock-findings.md`.
- Landing campaign (C/D) complete and recomputed: 500,000 targets, 1,000,000 landings,
  419,007 attested deterministic exits. Shard exit codes explained from the records in
  `qualification-evidence/shard-exit-codes-and-recovery.md`; digest inversion scored
  7000 agree / 0 disagree.
- Overshoot detection and recovery demonstrated: 113 of 113 recovered at margin 3072,
  4000 of 4000 exact on the campaign's overshot target at the sealed margin.

## Box state now

Kernel `6.18.35` (patched). SMT off, nmi_watchdog 0, governor performance,
`spec_store_bypass_disable=on`, SpecLockMap disabled, AVIC off, md1 resync frozen,
`perf_event_max_sample_rate` 100000000 with `perf_cpu_time_max_percent` 0.
Cores 1,3,5,7,9,11,13,15 isolated. `/etc/default/grub` still carries the patched-kernel
edits; `bash /root/kclose2.sh` restores the pristine file and names the stock release
`6.12.95` explicitly rather than deriving it from `uname -r`.

## Running

- The attested-exit supplement, since 07:43Z. 15 shards (`ae3-instr`, cores 0,1,2,4-15),
  `--margin 16192 --min-target 16193 --arms 24000 --replay --retries 3`. Records
  `/root/qual-evidence/stage2/supplement/core<N>.json`, PIDs in `.../supplement/pids`,
  per-minute counts in `.../supplement/progress.log`. About 4,970 arms a minute. Stop at
  about 301,000 arms, which puts total attested exits over a million; kill by PID from
  the `pids` file.
- `close-a.sh` on core 3 since 08:04Z, output `/root/qual-evidence/stage2/close-a.out`
  and `patched-close.log`. The enforcement probes are done and the single-step records
  are byte-identical to the stock ones; `ae5-gate --reps 1000` is still running.

## Next

1. Poll the supplement to about 301,000 arms, kill by PID, then
   `python3 /root/campaign-analysis.py /root/qual-evidence/stage2/supplement` and
   `digest-validate.py`. Report the isolated cores and the co-tenant cores separately.
2. `bash /root/ceiling-control.sh` on a quiet box: drops the sampling ceiling, shows
   `check` refuse, restores it, shows `check` accept. It changes a kernel setting every
   measurement depends on, so never while anything is running.
3. Write up the patched-kernel enforcement half from `patched-close.log`.
4. Item 8: `bash <scratchpad>/pull-campaign.sh` (gzips the shard files then rsyncs),
   `bash /root/kclose2.sh`, reboot, poll ssh, `bash /root/posture.sh`,
   `bash <scratchpad>/ship.sh`, `bash /root/stage0-recheck.sh`, rsync, final commit
   (no push), final report from `<scratchpad>/report-draft.md`.

## Recorded failures so far

One class, not two: every failing arm is an overshoot, and every one was refused rather
than accepted as a landing. Six of them across 838,014 arms, one in 139,669. Two are
first arms, skids 27,816 and 37,595, which the record describes on its face. Four are
replay arms, skids 29,884 / 50,432 / 52,737 / 56,725, which the record does not describe
and which were recovered by inverting the state digest; the inversion is scored 7,000
agree and 0 disagree against records that state the landing outright. So the guest-mode
maximum is 56,725, not the 37,595 the records show unaided. In every case the other arm
of the same target landed exactly on the target, and re-arming recovers: 6 of 6 in the
campaign and 113 of 113 in the demonstration at a deliberately small margin.
`overshoot-anatomy.md` and `shard-exit-codes-and-recovery.md` are the write-ups.
