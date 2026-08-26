# The time-stamp intercepts, and what the opt-in advertises on this chip

`amd-determinism-kernel-gap.md` records that the determinism series wired its
instruction intercepts on one vendor only, so the must-trap demonstration stage 2
specifies could not be run here. Two thirds of that gap is now closed and the
remaining third is not a gap.

Three patches, kept with the series at
`consonance/vmm-backend/kvm-patches/patches/`:

- `0006` is the SVM analogue of the force-exit plus the capability advertisement.
- `0007` turns the single enable bit into a per-class mask, so a host can advertise
  the classes it can cover instead of claiming all of them.
- `0008` sets `INTERCEPT_RDTSC` and `INTERCEPT_RDTSCP` on a VM that asked for the
  time-stamp class.

The randomness class stays absent, because SVM has no control for it. That is the
subject of `amd-rdrand-not-interceptable.md` and it is a property of the part.

## What the chip answered

`ae7-rdtsc.c` asks two questions on silicon rather than from the source: does the
opt-in tell the truth about what this vendor covers, and do the two time-stamp
instructions actually reach userspace. The guest installs a real-mode `#UD` handler,
so an instruction that faulted and an instruction that ran are distinguishable rather
than one of them hanging. The control is the same guest in a VM that did not opt in.

`rdtsc-svm.json`, `pass` 1:

- `supported_mask` is `0x5` — the time-stamp class and the preemption class, and not
  the randomness class. This is the host stating its own coverage.
- `enable_errno` is 0 for the time-stamp class and `EINVAL` for the randomness class,
  for all three classes together, for zero, and for an undefined bit. A request for a
  class this vendor cannot cover is refused rather than quietly narrowed.
- In the opted-in VM the guest read back `0x1122334455667788` from `RDTSC` and
  `0x99aabbccddeeff00` with `ECX` `0x5a5a5a5a` from `RDTSCP`: the sentinels userspace
  supplied, over two `KVM_EXIT_DETERMINISM` exits.
- In the control VM the guest read `0x3b1b4` from `RDTSC`, a host counter value, and
  took zero determinism exits. `RDTSCP` raised `#UD` there, which the handler recorded
  rather than hanging on.

So on this chip the deterministic backend gets `RDTSC` and `RDTSCP`, and it is told
plainly that it does not get `RDRAND` or `RDSEED`.

## The advertisement is what the backend should report

`0007`'s mask is the only thing that knows which classes a host can cover, and the
`vmm-backend` capability report used to ignore it: `patched_capabilities()` returned
`deterministic_rng: true` for any host that took the patched path. On this chip that
was false, and a caller asking whether its guest's entropy is seeded would have been
told yes. The report is now derived from the mask the host granted. See
`nested/README.md`.
