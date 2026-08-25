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
  is a recorded overshoot. Its replay, the same target moments later, landed exactly.
- core 9, target 64861: the first landing was exact, and the replay of the same target
  produced a different landed digest. That digest was later inverted to a work count -
  see `overshoot-anatomy.md` - and it says the replay stopped at 116241, a skid of
  59572. So this failure is the same kind as the one above, not a second kind.

Core 3, the isolated one, was clean in that pilot and in every solo run: 4800 solo
landings with a maximum skid of 5642.

`isolcpus`, `nohz_full` and `rcu_nocbs` were then widened to all eight cores and the
box rebooted into the patched kernel. The same pilot re-ran clean twice: 8000 of 8000
landings exact at margin 8192 with a maximum skid of 6578, and 8000 of 8000 at the
pack's sealed margin of 16192 with a maximum skid of 5370.

Isolation changes the rate by orders of magnitude and does not remove the behaviour.
The full campaign that followed, all eight cores isolated, still produced five late
overflows in about a million arms. So the finding has two halves, and the second is the
one that decides the verdict:

- a core that still takes the periodic tick and runs its own RCU callbacks makes a late
  overflow common enough to see in a few thousand arms, and the pack's `core-pinning`
  host condition is what buys that back;
- a late overflow is never a wrong landing. The arm is refused, and re-arming the same
  target lands exactly. That held for both failures in this pilot and for all five in
  the campaign.

The original command line is saved on the box at
`/root/qual-evidence/stage2/default-grub.before-wide-isolation` and is restored before
the box returns to the stock kernel, so stage 0 re-runs against the same host
conditions it was sealed with.
