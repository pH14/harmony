# Guest-kernel patches (Linux diffs — the GPL-2.0 kernel-patch exception)

This directory is the **designated guest-kernel patch directory**: everything
under it is a **diff against the pinned Linux kernel tree**
(`versions.lock: KERNEL_VERSION`), applied by the architecture-specific kernel
build right after `extract_kernel`, before the config merge.

**Arch-scoped layout.** Patches are split by target architecture into
`patches/x86/` and `patches/arm64/`. Each vendor's kernel build consumes **only
its own subdirectory**, so the two series apply independently and never collide
on a patch number (both arches carry a `0002-*`) — the flat `patches/[0-9]*`
glob that would have crossed the vendors is retired (hm-0dst, tribunal F7).

**Licensing.** Repository policy is first-party source = `AGPL-3.0-or-later`.
Kernel diffs are the one exception: their content is Linux-kernel code and
carries the kernel's `GPL-2.0` — permitted **only in diff form, only under a
designated patch directory** (this one for the guest kernel;
`consonance/vmm-backend/kvm-patches/patches/` for the host KVM modules —
whose README states the same bedrock GPL-2 no-copy discipline). Standalone
first-party `.c` files are not a permitted form; do not add one here.

**Application discipline.** The x86 build (`build-kernel.sh`) applies every
`patches/x86/[0-9][0-9][0-9][0-9]-*.patch` in lexical order with `patch -p1`,
guarded by a reverse dry-run — an exactly-applied tree is skipped (idempotent
re-builds on the persistent extracted tree), a pristine tree is patched, and a
drifted/partially-patched tree fails loudly (remove the extracted tree under
the build root and rebuild). The arm64 build (`build-arm64-kernel.sh`) applies
the `patches/arm64/` series (0002→0003→0004, in that order — the later patches
modify files the first creates) against a tree re-extracted pristine on every
run, each patch guarded by a forward dry-run.

**Regenerating a patch** (a kernel version bump, or editing the pvclock
clocksource): extract the pinned tree (on Linux or the linux/amd64 container —
a kernel tree cannot extract on a case-insensitive filesystem), apply the
current patch, make the edits in-tree, and `diff -u` the touched files against
a pristine extract into the patch file, keeping the explanatory preamble
(`patch` ignores everything before the first `---` header). Then re-run the
counter-opcode gate: the x86 pvclock clocksource deliberately executes **two**
`rdtsc` instructions (the pre-registration anchor freshener and the
post-registration clock advance the Δ refresh arms off), accounted in
`../rdtsc-allowlist.txt` — an edit that changes that count must update the
reviewed allowlist entry, and `run-tests.sh` must be re-run to regenerate
`../MANIFEST.sha256` (the diff changes the built image by construction). The
arm64 series carries no allowlist: its gate rejects the kernel if **any** live
generic-counter read or LL/SC opcode survives into the published image.

Each patch describes itself in its preamble (everything before the first
`---` header); the series listings are `ls patches/x86/` and `ls patches/arm64/`.
