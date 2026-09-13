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

The common series owns the architecture-independent Harmony character device,
observation handles, and process parking ABI. The device allocates and bounds
its shared pages in the kernel; platform userspace interacts through the
stable character-device interfaces.

## Architecture series

The x86 series supplies the paravirtual clock, counter confinement and
emulation, syscall tick, and x86-specific clock and trap plumbing. Its parking
patch leaves the generic process device in `common/` and contributes only the
x86 syscall hook.

The arm64 series supplies the exit-count clock page, LSE-only atomic contract,
virtual clock event, fixed counter and cache topology, interrupt handling,
canonical state, counter trap switch, and the arm64 syscall parking hook.

After a clock or trap patch changes, run the matching instruction reachability
scan and update its reviewed allowlist when the deliberate instruction count
changes. After any series change, run `test-patch-series.sh` and the platform
image checks. Generate a patch from a pristine extract of the pinned source;
preserve its explanatory preamble before the first diff header.
