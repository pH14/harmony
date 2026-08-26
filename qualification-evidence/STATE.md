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

Gone. The machine was released after the nested phase and its address reassigned; the
address above no longer reaches it, and an ssh attempt will present an unfamiliar host
key. Everything that phase produced is committed under `nested/` and `rdtsc/`.

Before that, at the close of the first phase, it was left on kernel
`6.12.95+deb13-amd64`, the stock Debian kernel the pack seals, booted from the restored
pristine `/etc/default/grub` with the GRUB default pinned to it in the two-level submenu
form. Posture re-applied and recorded in `qualification-evidence/box/posture-close.txt`.

That kernel had to be reinstalled: item 6's build dependencies pulled Debian's
`linux-image-amd64` forward to 6.12.101 and removed 6.12.95's image. See
`stock-kernel-moved-under-the-program.md`.

The nested phase then ran on a rebuilt 6.18.35 host with kernel patches `0006` through
`0009` applied; `nested/verify/00-posture.txt` records the posture it ran under.

## Running

Nothing.

## Next

Both programs are complete. Item 2 and most of item 3 have since been closed by the
nested phase; `nested/README.md` is its report. What is left:

1. The `chunks_exact` sites that fail the clippy gate on Linux with current stable, and
   the unpinned toolchain behind them: `linux-clippy-on-current-stable.md`, filed as
   issue #194.
2. Choosing a smaller landing margin, which needs an automatic re-arm that does not
   exist. `run_until` raises `SkidExceeded` and stops; recovery means restore-and-retry
   above the backend trait. `backend-margin-not-from-the-pack.md` has the cost curve.
3. A real CPUID model. Every call site passes an empty `CpuidModel::default()`, so a
   guest on this chip is told the randomness instructions exist and can execute them.
   Masking does not enforce anything on its own, but leaving the bits set advertises
   what the host cannot intercept: `amd-rdrand-not-interceptable.md`.
4. `count_offsets` and `single_step.work_per_step` stay absent until the suite's stage 2
   is built.
5. Gates and checks that need a Linux host, listed in `nested/README.md`: the full
   workspace gates, regenerating `consonance/vmm-backend/tests/public-api.txt`, and the
   live contract exam against the per-backend margin.
6. Verifying the AMD single-step fallback: patch `0010` and the backend change beside
   it are unbuilt and unrun, and TF stepping has never landed on this vendor through
   the backend. Issue #196.

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
