# Guest-kernel patches (Linux diffs — the GPL-2.0 kernel-patch exception)

This directory is the **designated guest-kernel patch directory**: everything
in it is a **diff against the pinned Linux kernel tree**
(`versions.lock: KERNEL_VERSION`), applied by `../build-kernel.sh` right after
`extract_kernel`, before the config merge.

**Licensing.** Repository policy is first-party source = `AGPL-3.0-or-later`.
Kernel diffs are the one exception: their content is Linux-kernel code and
carries the kernel's `GPL-2.0` — permitted **only in diff form, only under a
designated patch directory** (this one for the guest kernel;
`consonance/vmm-backend/kvm-patches/patches/` for the host KVM modules —
whose README states the same bedrock GPL-2 no-copy discipline). Standalone
first-party `.c` files are not a permitted form; do not add one here.

**Application discipline** (`build-kernel.sh`): each patch is applied with
`patch -p1`, guarded by a reverse dry-run — an exactly-applied tree is
skipped (idempotent re-builds on the persistent extracted tree), a pristine
tree is patched, and a drifted/partially-patched tree fails loudly (remove
the extracted tree under the build root and rebuild).

**Regenerating a patch** (a kernel version bump, or editing the pvclock
clocksource): extract the pinned tree (on Linux or the linux/amd64 container —
a kernel tree cannot extract on a case-insensitive filesystem), apply the
current patch, make the edits in-tree, and `diff -u` the touched files against
a pristine extract into the patch file, keeping the explanatory preamble
(`patch` ignores everything before the first `---` header). Then re-run the
counter-opcode gate: the pvclock clocksource deliberately executes **two**
`rdtsc` instructions (the pre-registration anchor freshener and the
post-registration clock advance the Δ refresh arms off), accounted in
`../rdtsc-allowlist.txt` — an edit that changes that count must update the
reviewed allowlist entry, and `run-tests.sh` must be re-run to regenerate
`../MANIFEST.sha256` (the diff changes the built image by construction).

## Patches

- `0001-x86-harmony-pvclock-work-derived-clocksource.patch` — task 110
  (`docs/PARAVIRT-CLOCK.md`): `CONFIG_HARMONY_PVCLOCK`, the kvmclock-shaped
  clocksource with the interpolation deleted; sched_clock through the same
  seqlock page read; TSC marked unstable once the page is live; one-shot
  doorbell registration bracketed by the two deliberate rdtsc traps.
  Runtime-inert without the `harmony_pvclock` kernel parameter.
- `0002-x86-harmony-character-device.patch` — R-L3/task 43: built-in
  `/dev/harmony`, attributed JSON emit, and deterministic entropy transactions
  over the existing doorbell. It adds no host protocol and no new exit type.
