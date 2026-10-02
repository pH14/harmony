<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Guest-kernel patch series

These diffs apply to the Linux version pinned in `../versions.lock`. The
platform build applies every numbered patch in `common/` first, then every
numbered patch in the selected architecture directory. The series stamp
includes directory names, patch names, and SHA-256 values, so a partially
applied or changed tree is rejected and must be re-extracted.

Kernel patch content remains under the kernel's GPL-2.0 license and is kept in
diff form under the repository's kernel-patch exception. First-party code
elsewhere uses the repository license.

## Common series

The common series owns the architecture-independent Harmony character device
and observation handles. The device allocates and bounds its shared pages in
the kernel; platform userspace interacts through the stable character-device
interfaces.

## Architecture series

The x86 series supplies the paravirtual clock, counter confinement and
emulation, syscall tick, idle port, and x86-specific clock and trap plumbing.
Its KVM host-counter patch uses the outer VMM's intercepted TSC MSR for shared,
LAPIC, PMU and nested-VMX counter reads, and disables the optional VMX hardware
preemption timer by default. KVM hosting is compiled only in the nested-host
profile; the ordinary guest kernel continues to omit those paths.
The SVM boot extensions consume the frozen CPUID frequencies, accept the
empty type-1 PCI bus after its latch check and omit physical northbridge
initialization and FCH reset-status diagnostics. They require AMD KVM, AMD
identity, SVM and the Harmony clock;
ordinary guest configurations compile them out.

The arm64 series supplies the exit-count clock page, LSE-only atomic contract,
virtual clock event, fixed counter and cache topology, interrupt handling,
canonical state, counter trap switch, and idle register. Its IRQ-unmask fence
exits only while the clock page's `irq_pending` word says a due deadline waits
for the unmask.

After a clock or trap patch changes, run the matching instruction reachability
scan and update its reviewed allowlist when the deliberate instruction count
changes. After any series change, run `test-patch-series.sh` and the platform
image checks. Generate a patch from a pristine extract of the pinned source;
preserve its explanatory preamble before the first diff header.
