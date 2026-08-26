# Running the qualification suite inside a virtual machine

The suite runs end to end inside a virtual machine on this chip and reaches the
same verdict it reaches on the metal underneath. Stage 0 confirms, stage 1
measures, and the report recomputes to a pass. Thirty-six checks on each side,
all passing, with no check present on one side and missing on the other.

Both runs used the same binary and the same sealed pack, `det-zen3-v1`.

## What the two runs measured

The maximum skid over 250,000 arms per payload class, 1,250,000 arms per run:

| payload class  | metal | in a virtual machine | difference |
| -------------- | ----: | -------------------: | ---------: |
| branch_dense   |   397 |                  402 |      +1.3% |
| call_ret       | 1,591 |                1,608 |      +1.1% |
| locked         | 1,175 |                1,165 |      -0.9% |
| loop_backedge  | 8,013 |                8,040 |      +0.3% |
| straight_line  |   500 |                  502 |      +0.4% |

Every arm was delivered exactly once on both sides: zero lost, zero duplicated,
zero premature, zero unaccounted, and zero over the pack's sealed margin of
16,192. The largest skid either side produced leaves the margin with about twice
the headroom it needs.

Exactness: sixteen payload-and-condition combinations on each side, every one
with no mismatch against the oracle and a fixed offset that held across all 512
repetitions. The counter is as exact inside a virtual machine as it is on metal,
under a quiet core, a co-tenant, and memory pressure alike.

The snapshot round-trip check passed inside the virtual machine as well, so the
guest window stage 1 opens for it works two levels down.

## What a guest cannot see, and what was done about it

Stage 0 checks eleven standing conditions on this chip. Five of them describe the
physical machine, and a guest either cannot read them or reads its own copy:

- the frequency governor — a guest has no frequency driver at all
- the simultaneous-multithreading policy — a guest is not told which of its
  processors share a core
- which core the measurement is pinned to — the guest's core numbering is its own
- speculative lock mapping in `LS_CFG`, on each processor — KVM refuses the
  register to a guest

These are recorded as acceptances in `guest-dispositions.toml`, each naming the
reading it accepts and why. Stage 0 marks a matching row dispositioned instead of
undecided, and the reason travels in the run's records. An acceptance names the
reading, so a machine that changes underneath a run stops matching and the
deviation goes live again; an acceptance that matches nothing is a refusal.

Speculative lock mapping is the one that carries real weight, and the run does not
rest on the acceptance alone. The suite's behavioural probe runs inside the guest
and reads zero speculative lock-map commits over a `lock add` loop, with the work
clock nonzero on the same run. That is the behaviour the register produces when
the workaround is in force, observed rather than asserted.

Four other deviations turned out to be the guest being set up wrong rather than
anything a guest cannot do, and were fixed rather than accepted:

- **Core isolation.** A single-processor guest cannot isolate its only processor.
  The guest now has two, boots with `isolcpus=1 nohz_full=1 rcu_nocbs=1`, and the
  virtual processor behind guest cpu1 is pinned to the host's isolated core 3
  while housekeeping goes to a different host core.
- **The speculative-store-bypass mitigation mode**, set on the guest's command
  line.
- **The kernel's ceiling on sampling interrupts.** The kernel refuses to raise the
  sample rate while the throttle is off, so the rate has to be set first and the
  throttle second. In the other order the write fails and the ceiling silently
  stays at the stock 100,000.
- **The KVM module identity.** Stage 1 opens a guest of its own for the snapshot
  round-trip check, so the module doing that is part of what the run measures.
  The guest image now carries the same build the host runs and the pack names,
  rather than the older one it shipped with.

## Two things this changed in the suite

**A way to record an acceptance.** The report format has carried a `disposition`
field from the start, and the specification says every row is either confirmed or
explicitly dispositioned, but nothing could set one — every deviation was
undecided forever. `run` and `check` now take `--dispositions <path>`.

**A way to reseal a pack.** A pack's hash covers its own content, so any change to
a recorded value makes the pack fail to load until it is resealed, and nothing
could reseal one. `cpu-qualification seal --pack <path>` rewrites the `pack_hash`
line and leaves every other byte alone.

Stage 0 also no longer aborts when a register cannot be read. It used to stop the
whole condition enumeration at the first unreadable register and report nothing
about the conditions it could have checked. An unreadable register is now a
reading like any other, and it still fails the comparison against the pack.

## The pack was resealed

`det-zen3-v1` recorded the KVM module identities from before the nested-counting
patch. Both modules were rebuilt with that patch, so the two rows were updated to
the build the host now runs and the pack resealed:
`61a0829b…` to `08b8fd47f4a5a2bcda0b1b630928d08bed2c536656864a7351fbd4ed45f49564`.

No measured constant moved. The patch changes how KVM emulates a counter for a
guest; stage 1's own counter is a host counter opened directly, and the stage-1
run above reproduces the pack's margin from scratch on the rebuilt modules.

## Files

- `suite/metal/` — stage 0 and stage 1 record streams and the recomputed report
  from the metal run
- `suite/vm/` — the same from the run inside the virtual machine, plus the stage-0
  rows with their acceptances
- `guest-dispositions.toml` — the recorded acceptances
- `suite/boot-l1-iso.sh` — boots the guest with two processors and pins each
  virtual processor to its host core
- `suite/l1-run.sh` — what runs inside the guest

The per-arm record streams are gzipped; `report --evidence-dir` reads them
uncompressed.
