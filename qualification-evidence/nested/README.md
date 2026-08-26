# Running the machinery inside a virtual machine, and three fixes it forced

`FINAL-REPORT.md` closed the qualification with the machinery running on metal. This
phase asked whether it also runs when consonance is itself a guest, which is what a
developer laptop, a shared runner and a cloud instance all are. The answer is yes on
this chip, after one kernel patch, and the counting is exact two levels down.

Getting there ran into three things in this repository that were wrong rather than
merely unmeasured. All three are fixed here; two of them were already recorded as
findings this program deliberately did not fix.

## What was measured

Three documents, in the order the work happened:

- `nested-on-this-chip.md` — the work clock does not count a nested guest correctly
  without patch `0009`, why, and the A/B that proves the patch.
- `landing-in-a-virtual-machine.md` — 5,000 arms with replay on both sides of the
  same chip: identical landings, unchanged skid, and what it costs.
- `the-suite-in-a-virtual-machine.md` — the qualification suite end to end inside a
  virtual machine, reaching the same verdict as the metal underneath.

The short version. Counting a nested guest was broken in software, not in silicon:
KVM listed the AMD event-select host/guest filter bits as reserved and stripped them,
and pinned the backing event to count all guest execution, so a guest asking to count
only *its* guest counted its whole self. Patch `0009` keeps the bits and starts and
stops the backing event at the nested transitions. After it, every test that passes on
metal passes inside a virtual machine.

Landing works there too and lands on the same states: 5,000 arms, zero digests
differing, skid distribution unchanged (median 2,906 against 2,907). It costs 4.1
times more, 367 milliseconds against 90, and the cost is the single-stepping — 29.6
microseconds a step under nesting against 7.2 on metal.

The suite reaches the same verdict on both: 36 checks each side, all passing, none
present on one side only. Five host conditions describe the physical machine and a
guest cannot read them; those are accepted with recorded reasons in
`guest-dispositions.toml`, and the speculative-lock-map one is backed by the suite's
own behavioural probe running inside the guest rather than by the acceptance alone.

## Three fixes

**The backend claimed a determinism guarantee this vendor cannot give.**
`patched_capabilities()` returned `deterministic_rng: true` for any host on the
patched path. SVM has no `RDRAND` or `RDSEED` intercept control, so on this chip that
was false — see `../amd-rdrand-not-interceptable.md` for the silicon proof, and
`../rdtsc/the-time-stamp-intercepts-on-svm.md` for the host reporting its own coverage
as the time-stamp and preemption classes only. The enable path already asked for the
granted set and the report ignored it. The report now derives from the granted mask.
Confirmed on the box: the patched backend reports `rng=false tsc=true`.

**The arm-early margin was a constant from another chip.**
`../backend-margin-not-from-the-pack.md` recorded this and left it, because that
program's terms allowed code changes only to the pack and to defects the box exposed
in the suite. It is a stop, not a slowdown: at 256 on this chip `run_until` raises
`SkidExceeded` on the first landing, and the live contract exam does exactly that.
`SKID_MARGIN` is now `DEFAULT_SKID_MARGIN`, documented as Coffee Lake's number, with
`KvmBackend::set_skid_margin` beside it; the live exam reads the running chip's sealed
margin from its pack.

**Stage 0 stopped at the first register it could not read.**
Inside a guest, `LS_CFG` is refused by KVM, and the suite aborted the whole condition
enumeration there and reported nothing about the conditions it could have checked. An
unreadable register is now a reading like any other, and it still fails the comparison
against the pack.

## Two mechanisms the suite was specified to have and did not

**Recorded acceptances.** `docs/CPU-QUALIFICATION.md` has always said every stage-0 row
is confirmed or explicitly dispositioned, and `Row::disposition` has existed from the
start with nothing able to set it, so every deviation was undecided forever. `run` and
`check` now take `--dispositions <path>`. An acceptance names the reading it accepts,
not just the condition, so a machine that changes underneath a run stops matching and
the deviation goes live again; an acceptance that matches no deviating row is a
refusal.

**Resealing a pack.** A pack's hash covers its own content, so editing any measured
value made the pack fail to load, and nothing could reseal one.
`cpu-qualification seal --pack <path>` rewrites the `pack_hash` line and leaves every
other byte alone.

## The pack was resealed

`det-zen3-v1` recorded the KVM module identities from before patch `0009`. Both modules
were rebuilt with it, so the two rows were updated and the pack resealed to
`08b8fd47f4a5a2bcda0b1b630928d08bed2c536656864a7351fbd4ed45f49564`. No measured constant
moved. The patch changes how KVM emulates a counter for a guest; stage 1's own counter
is a host counter opened directly, and the stage-1 run in
`the-suite-in-a-virtual-machine.md` reproduces the pack's margin from scratch on the
rebuilt modules.

## A compatibility break worth knowing about

Patch `0007` redefined `args[0]` on `KVM_CAP_X86_DETERMINISTIC_INTERCEPTS` from a single
enable bit to a class mask. A caller written against the old form asks for the
time-stamp class alone and finds out at the first ioctl the missing class gates, not at
the enable. That is how the landing harness failed here — `ENABLE_CAP` succeeded and
`KVM_ARM_PREEMPT_EXIT` returned `EINVAL`. Callers should read the supported set with
`KVM_CHECK_EXTENSION` and ask for that.

## What is not settled

- The deep skid tail inside a virtual machine. 5,000 arms cannot reach where the
  metal campaign's million landings went.
- The guest here has two virtual processors, one of them isolated and backed by the
  host's isolated core. Nothing establishes what several contending virtual processors
  on one host core would do.
- The outer kernel is always the patched build. Nothing establishes what a stock outer
  hypervisor, or a cloud vendor's, would do.
- The live contract exam has not been re-run since the margin fix. The box was spun
  down first.
