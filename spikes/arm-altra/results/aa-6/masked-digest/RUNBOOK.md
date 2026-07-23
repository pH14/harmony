<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# AA-6 masked-register-digest lane — turnkey box runbook (hm-3bwm / task 138)

The **named condition** on the AA-6 LinuxGuest PROVISIONAL→full-GO upgrade. Prove at gate
scale that change #4's `console + vGIC` narrowing is **exactly-and-only** the disclosed
AA-5(c) stack-ASLR residual `{x29, SP}` (hm-of6t F12), **not** masking an injection-path
register divergence.

Apparatus is built and **portably green**; this is the single on-silicon step remaining.
**The condition is OPEN — not met** until the ≥1000-rep run below is PASS. The ARM box was
spun down (Paul, 2026-07-22), so this lane is deferred to the next ARM window.

## Preconditions on the box

- Host on the patched kernel: `uname -r` = `6.18.35-aa3preempt` (if reverted, grub-reboot
  boot-once into aa3preempt; stock stays default). The `linux-boot` path refuses to fall
  back to stock (the exact-work clock rides the `KVM_EXIT_PREEMPT` patch).
- The spike built on the box: `cd ~/harmony/spikes/arm-altra && cargo build --release -p arm-harness`
  (git push to the box is classifier-blocked — **bundle-transfer** this branch instead).
- The pinned AA-5(c)/AA-6 LinuxGuest artifacts staged and matching their committed pins:
  - `Image` sha256 `d0161a7d41309b6e9139534d99c8c3d24152c0b10c06b4829443402698c5aefe`
  - `initramfs` sha256 `604733be3338ac55cc0f387ba55b7b6b31250d158761ca2cc422cf2e37d08573`

  Point `$IMAGE`/`$INITRAMFS` at their box paths; the lane passes `--image-sha256` /
  `--initramfs-sha256`, so the harness refuses any artifact that does not match the pin.

## Pinning (docs/BOX-PINNING.md)

The Altra N1 has **no SMT**, so a single dedicated core is the correct pinned posture. Core
**60** is what the merged AA-6 run used; the runner `taskset -c "$CORE"`-pins every boot.
Keep the SMT sibling notion N/A here (no hyperthreads to leave idle on the Altra).

## Run it

From `~/harmony/spikes/arm-altra`, with `$IMAGE`/`$INITRAMFS` exported if not at the
defaults (`$HOME/harmony-linux/Image`, `$HOME/harmony-linux/initramfs.cpio.gz`):

### 1. Smoke-fire ONCE — a ~20-rep batch first (Environment §). Eyeball the verdict.

```sh
bash host/aa6-masked-digest-lane.sh 20 60 smoke
# => RESULT: PASS (8 of 8 checks passed) -> results/aa-6/masked-digest-smoke/verdict.json
```

Confirm `injected_landed_digest` is a real `sha256:…` (not `none` — the injection fired) and
`masked-digest-bit-identical` PASS across the 20 reps before spending the full batch.

### 2. The gate — ≥1000 reps, detached.

```sh
nohup setsid bash host/aa6-masked-digest-lane.sh 1000 60 gate </dev/null \
    >~/aa6-masked-gate.log 2>&1 &
# progress every 100 reps in the log; on completion:
#   RESULT: PASS (8 of 8 checks passed) -> results/aa-6/masked-digest-gate/verdict.json
```

The runner writes `results/aa-6/masked-digest-gate/`: `config.json` (the injection config,
recorded explicitly), `rep-NNNN.stdout` per rep, `console-first.bin` (provenance),
`verdict.json`. **Commit the evidence dir promptly** (the box was account-wiped once on
2026-07-20).

## What is being compared

Each `linux-boot` runs the **AA-6 injection configuration** — the same as the merged PR #139
matrix: `--inject-ppi 22 --inject-at-work 1 --seed 1` (PPI-22 pending latch at the first
exact refresh landing, work 10,000,000), `--skid-margin 1024`, `--condition pinned-solo`.
The summary line now emits:

- `masked_regs_digest` — the full register file MINUS exactly `{x29, SP}` at the success
  landing (host-time counters already excluded). **This is the digest the lane compares.**
- `injected_landed_digest` — the same masked digest at the **injection Moment** (hm-fiqo).
- `masked_excluded_gprs=x29:0x603000000010003a,SP:0x603000000010003e` and
  `masked_excluded_host_time=CNTPCT_EL0,CNTPCTSS_EL0,CNTVCTSS_EL0,KVM_REG_ARM_TIMER_CNT` —
  the exclusion set **enumerated**, not implied.

`aa6-masked-digest-check.py` (invoked by the runner) requires: ≥`--min-reps` reps; the mask
enumerated and **exactly** `{x29, SP}`; the injection **fired** (witness ≠ `none`); the
pinned artifacts; and both `masked_regs_digest` and `injected_landed_digest`
**bit-identical across every rep**.

## Disposition (tasks/138)

- **All reps identical ⇒ named condition MET.** Record the evidence + a condition-met note in
  `docs/ARM-ALTRA.md` §AA-6. Do **not** flip PROVISIONAL→GO yourself — that is Paul's ruling;
  the foreman escalates it with this evidence.
- **Any divergence in the masked digest** (which already excludes `{x29, SP}`) ⇒ a register
  *outside* `{x29, SP}` moved same-seed — a possible injection-path register divergence the
  `console + vGIC` narrowing was masking. **P0-class STOP:** commit the evidence, PARK,
  escalate. The checker enumerates every distinct digest with counts (never hidden). **Never**
  widen the mask, **never** narrow the digest further to reach green.
