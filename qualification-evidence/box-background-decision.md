# The md1 resync, and what was done about it

At 2026-08-25T01:49Z the box showed load average 1.04 while otherwise idle.
`ps aux --sort=-%cpu` showed no user process: no leftover cargo, rustc, or
provisioning job. The load came from `md1_resync`, the kernel thread rebuilding
the RAID1 mirror `md1` (nvme0n1p3 + nvme1n1p4, 1,869,988,864 blocks). At that
moment it was actively copying:

    [====>................]  resync = 24.4% (456576384/1869988864)
    finish=116.0min speed=202955K/sec

Both arrays reported `[UU]`, so the mirror was readable throughout; the resync
was a background consistency rebuild, not a degraded-array recovery.

Decision: the resync was **frozen**, not throttled and not left to finish.

    echo frozen > /sys/block/md1/md/sync_action

Reasons:

- 116 minutes of sustained 200 MB/s NVMe traffic across the whole run window
  would have put device interrupts and memory-bus traffic underneath every
  clean baseline measurement. Stage 1 characterises interference deliberately,
  with named probes; uncontrolled background I/O during the baseline runs would
  make the baseline and the interference arm indistinguishable.
- Throttling only lowers the rate; it lengthens the window and leaves the
  traffic present for the whole program.
- Letting it finish costs about two paid box-hours before any measurement
  starts.
- This is a scratch qualification box. Array redundancy is not a requirement of
  the program, and `frozen` is reversible with a single write.

`frozen` rather than `idle` because `idle` lets md restart the resync on its
own; `frozen` holds it stopped. md restarts a resync on array assembly, so the
freeze is re-applied after every reboot, alongside the other volatile posture
settings.

Load average is not the quiet test used in this program. Each campaign records
the state of the core it pins to and what else was running on the machine at
the time.
