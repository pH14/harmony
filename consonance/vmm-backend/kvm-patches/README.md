<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Deterministic KVM patch series

This directory contains an opt-in three-patch series against the pinned Linux
kernel. The patches add a userspace exit and completion ABI for x86 RDTSC,
RDTSCP, RDRAND, and RDSEED, then enable the corresponding VMX exits for VMs
that request the capability. The default KVM behavior remains unchanged.

The VMM supplies the completion values: virtual time for counter reads and the
caller-seeded entropy stream for random reads. A VM that requests the feature
fails closed if the CPU controls or completion state are unavailable. The
series does not implement timing, preemption, performance-counter access, or
instruction stepping.

The patch files are Linux-kernel diffs and remain in diff form. Apply and build
them against the version named by `BUILD.md`; the resulting modules must match
the running kernel before loading them.

The ABI is summarized in `patches/README.md`; that file and `BUILD.md` are the
operational references for the series.
