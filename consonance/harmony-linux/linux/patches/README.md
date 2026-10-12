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
The KVM bitmap-drain extension invalidates the drained slot's shadow mappings
after automatic protection while the Harmony clock is active. This prevents
cached writable mappings from hiding later L2 writes on hosted Intel nesting.
It uses the existing KVM slot invalidation API, keeps RAM and the VM allocated,
and leaves manual-protection mode unchanged. The Nested Host job qualifies it
on the vendor its runner provides, VMX or SVM.

The arm64 series supplies the exit-count clock page, LSE-only atomic contract,
virtual clock event, fixed counter and cache topology, interrupt handling,
canonical state, counter trap switch, and idle register. Its IRQ-unmask fence
exits only while the clock page's `irq_pending` word says a due deadline waits
for the unmask.

The um series applies after the common series to the separate User-mode Linux
source tree. It makes SECCOMP userspace the only UML mode, charges virtual
time per system call and clock read, carries `/dev/harmony` to the host over
an inherited socket, seeds the random pool from the boot command line,
emulates the x86 time-stamp counter, keeps host child-process signals out of
the guest, fixes the guest's physical memory layout, and lets the host capture
and restore the kernel while it waits for a bridge answer.

The um-arm64 series applies after the common series to the pinned arm64 UML
RFC tree. It carries the um series with counter emulation for the arm64
counter and its frequency register (the stub then waits without the RFC's
counter-bounded spin), two RFC build fixes (host headers that compile against current
glibc and a stub without a frame pointer), zeroed floating-point state for
every new guest program, a copy of the host platform name that outlives the
host stack, and synchronous reaping of killed stubs.

After a clock or trap patch changes, run the matching instruction reachability
scan. After any series change, run `test-patch-series.sh` and the platform
image checks. Generate a patch from a pristine extract of the pinned source;
preserve its explanatory preamble before the first diff header.
