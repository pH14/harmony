# What an exact landing costs on this chip, and what it needs from the host

## The cost

An AE-3 landing arms the work clock at `target - margin`, takes the overflow as an
NMI that leaves the guest through the patched `nmi_interception` with
`KVM_EXIT_PREEMPT`, and then single-steps the guest until the work clock reads
`target` exactly. The single-stepping dominates: each step is a `KVM_RUN` returning
`KVM_EXIT_DEBUG` plus a counter read, about 7 microseconds on this box, and the
number of steps is `margin - skid`.

Measured on core 3 alone, patched 6.18.35, determinism posture applied:

| margin | landings | wall | landings/s | mean steps per landing |
|--------|---------|------|-----------|------------------------|
| 16192  | 800     | 72.5s | 11.0     | 13213 |
| 8192   | 4000    | 151.8s | 26.3    | 5222 |

The skid is tightly concentrated — over 2400 targets the median was 2912 and the 99th
percentile 3461 — so the step count is essentially the margin itself. A single core
therefore reaches the 1,000,000-landing floor in 10.6 hours at margin 8192 and 25
hours at 16192. That is a property of the mechanism, not of the box's speed: a
landing is a serial walk of the margin.

The campaign was spread across eight cores instead, which is the same primitive run
eight times independently rather than a change to it. Eight cores at margin 16192
deliver 84 landings a second, putting the floor at about 3.3 hours.

## The isolation requirement

The first eight-way pilot ran on cores 1, 3, 5, 7, 9, 11, 13 and 15 while only core 3
carried `isolcpus` / `nohz_full` / `rcu_nocbs`. Of 8000 landings, 7998 were exact.
The two failures were both on non-isolated cores and both a late overflow:

- core 13, target 30070, period 21878: the counter read 46657 when the preempt exit
  arrived — a skid of 24779, three times the 8192 margin and 16587 past the target.
  Once the work clock is past the target, single-stepping cannot reach it, so the arm
  is a recorded overshoot.
- core 9, target 64861: the first landing was exact, and the replay of the same target
  produced a different landed digest.

Core 3, the isolated one, was clean in that pilot and in every solo run: 4800 solo
landings with a maximum skid of 5642.

`isolcpus`, `nohz_full` and `rcu_nocbs` were then widened to all eight cores and the
box rebooted into the patched kernel. The same pilot re-ran clean twice: 8000 of 8000
landings exact at margin 8192 with a maximum skid of 6578, and 8000 of 8000 at the
pack's sealed margin of 16192 with a maximum skid of 5370.

The finding is that the exact-landing contract depends on the measurement core being
isolated. On a core that still takes the periodic tick and runs its own RCU callbacks,
the overflow NMI can arrive thousands of events late, and a landing that arrives late
is not recoverable. The pack already carries `core-pinning` as a host condition; this
says what the pinning has to be worth.

The original command line is saved on the box at
`/root/qual-evidence/stage2/default-grub.before-wide-isolation` and is restored before
the box returns to the stock kernel, so stage 0 re-runs against the same host
conditions it was sealed with.
