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

1. Wait for `/root/qual-evidence/stage2/campaign/progress.log` to say `alive=0`, then
   `python3 /root/campaign-analysis.py /root/qual-evidence/stage2/campaign`. It
   recomputes everything the verdict needs from the per-arm records, including the
   digest inversion that recovers what a replay arm did.
2. Size the supplement from that output. The item-7 floor is a million landings at
   `work == target` with the mechanism attested, and only the first arm's exit reason is
   in the campaign's records, so the attested count is about 419,000, not 838,000. Run
   `bash /root/supplement.sh <targets-per-core> supplement <cores...>` 16-wide with
   `ae3-instr`, which records the replay arm's exit reason. Cores 1-15 odd are the
   isolated population and 0-14 even are a co-tenant one, reported separately.
   Alongside it, `bash /root/repro-core9.sh <core>` reproduces seed 109 to index 15257.
3. `bash /root/overshoot-demo.sh` for the recovery rate at a deliberately small margin.
   The recovery itself is already demonstrated 5 times out of 5 in the campaign - see
   `overshoot-anatomy.md` - so this measures the rate, not the fact.
4. `bash /root/close-a.sh` while the patched kernel is booted: ae4-freeze, ae4-msr, the
   single-step driver in both modes, the RDRAND probe, and ae5-gate.
5. `bash /root/ceiling-control.sh` on a quiet box: drops the sampling ceiling, shows
   `check` refuse, restores it, shows `check` accept. It changes a kernel setting every
   measurement depends on, so never while anything is running.
6. Final pack edit. `skid.observed_max` source must say the maximum comes from campaign
   C and that 11,090 arms across C and A2 are unaccounted behind it. `skid.derivation`
   must carry both distributions labelled and say which one the margin derives from and
   why. `skid.overshoot` must carry the measured detection and recovery numbers. The
   margin stays 16192. Reseal with `bash <scratchpad>/reseal.sh`, run the five gates,
   commit.
7. Item 8: rsync (gzip the shard files on the box first), `bash /root/kclose2.sh`,
   reboot, poll ssh, `bash /root/posture.sh`, ship the pack, `bash /root/stage0-recheck.sh`,
   rsync, final commit, final report.

## Recorded failures so far

One class, not two. All five failing arms are overshoots; three of them are on the
replay arm, whose landing the record does not describe and which was recovered by
inverting the digest. Skids 27,816 / 37,595 / 50,432 / 52,737 / 56,725, so the
guest-mode maximum is 56,725 and not the 37,595 the record shows on its face. Every one
was rejected rather than accepted, and in every one the other arm of the same target
landed exactly on the target. `overshoot-anatomy.md` is the write-up.
