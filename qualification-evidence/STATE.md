# Resume state

Worktree: `/Users/phemberger/workspace/harmony/.claude/worktrees/agent-a98a1db9c86d06f8c`
Branch: `task/cpu-qualification-suite`. Box: `ssh -i ~/.ssh/id_ed25519 root@62.210.145.82`.
Scratchpad: `/private/tmp/claude-501/-Users-phemberger-workspace-harmony/a54e77d2-0ede-4a28-96d4-7ccc43759491/scratchpad`.

Never identify a box process by a `pkill -f` or `pgrep -f` pattern: it matches this
session's own argv. Use PIDs from the run's `pids` file and `kill -0`.

## Done

- Items 1, 2, 3, 4, 5 complete. Pack `docs/chips/det-zen3-v1.toml` sealed
  `3fe74869c247f973d09490eabde9ffba9d4759dd73e4f2bca6a25e7026af46cb`.
- Stage 1 campaign E passes: `report --evidence-dir /root/qual-evidence/stage1-E`
  exits 0, 36 of 36 checks. Independently recomputed outside the suite.
- Item 6 complete: kernel 6.18.35 built with the re-anchored SVM patch (kernel commit
  `52248ae11` in `/root/kbuild-618/linux-6.18.35`, diff saved at
  `qualification-evidence/box/stage2/amd-svm-reanchored.patch`), staged through the
  GRUB one-shot after a QEMU pre-flight, and booted. One of the two permitted patch
  attempts was used.
- Item 7 stock half complete: `qualification-evidence/stage2-stock-findings.md`.

## Box state now

Kernel `6.18.35` (patched). SMT off, nmi_watchdog 0, governor performance,
`spec_store_bypass_disable=on`, SpecLockMap disabled, AVIC off, md1 resync frozen,
`perf_event_max_sample_rate` 100000000 with `perf_cpu_time_max_percent` 0.
Cores 1,3,5,7,9,11,13,15 isolated. `/etc/default/grub` still carries the patched-kernel
edits; `bash /root/kclose2.sh` restores the pristine file and names the stock release
`6.12.95` explicitly rather than deriving it from `uname -r`.

## Running

- The landing campaign, since 04:26:09Z. 8 shards (`ae3-forceexit`, cores
  1,3,5,7,9,11,13,15), 62500 targets each, `--replay`, margin 16192 which is the
  pack's sealed value. Records `/root/qual-evidence/stage2/campaign/core<N>.json`,
  PIDs in `.../campaign/pids`, per-minute counts in `.../campaign/progress.log`.
  Rate about 2640 arms a minute; expected finish about 07:35Z.

## Next

1. Wait for `progress.log` to report `alive=0`, then
   `python3 /root/campaign-analysis.py /root/qual-evidence/stage2/campaign`.
   It recomputes arms, landings, exits through KVM_EXIT_PREEMPT, the failing arms,
   the guest-mode skid percentiles and the counts beyond the sealed margin, from the
   per-arm records rather than from any summary line.
2. Size the supplement from that output: the item-7 floor is a million landings that
   went through the deterministic exit, and only about 83.8 percent of arms arm an
   overflow at all (targets are uniform on [1,100000], margin 16192). Then
   `bash /root/post-campaign.sh <targets-per-core>`: seven cores run `ae3-instr`
   with `--retries 3`, while core 9 re-runs seed 109 to index 15257 under `ae3-diag`
   to reproduce the replay mismatch.
3. `bash /root/overshoot-demo.sh`. This is the named record the verdict turns on:
   part A arms at margin 3072 so overshoot is common and measures how often re-arming
   the same target recovers; part B re-arms the campaign's own overshot target 85981
   at the sealed margin 16192.
4. `bash /root/close-a.sh` while the patched kernel is still booted: `ae4-freeze` and
   `ae4-msr` (the AMD draft contract column names `kernel-tag = "v6.18.35"`), plus
   `ae5-gate --reps 1000` as supporting evidence. Stage 3 is out of scope.
5. Final pack edit: rewrite `skid.derivation` to record both skid distributions
   labelled and say which one the margin derives from and why, and `skid.overshoot`
   to carry the detection-and-retry story with the measured recovery numbers. The
   margin stays 16192; nothing here widens it. Reseal, gate, commit.
6. Item 8: rsync evidence, `bash /root/kclose2.sh`, reboot, poll ssh,
   `bash /root/posture.sh`, ship the final pack, `bash /root/stage0-recheck.sh`,
   rsync again, final commit, final report.

## Recorded failures so far

Two classes, both to be reported as rates with their denominators, never rounded away:

- Overshoot: 2 arms, both core 15. Index 28164 target 85981 skid 37595; index 35919
  target 27325 skid 27816. Both exceed the sealed margin 16192; the larger is 4.6x it.
  On an isolated core, so not a co-tenancy artifact.
- Replay-digest mismatch with a normal skid and an exact first landing: 3 arms, cores
  5, 9 and 11.

At 289571 arms judged this was 1 overshoot in 144785 arms and 1 mismatch in 96523.
