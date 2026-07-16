# BUILD.md — apply → build for the arm64 KVM_EXIT_PREEMPT draft patch

**Status: DRAFT, untested on silicon.** This recipe proves *applies + compiles*, nothing more.
It has never produced a booted host kernel. See `patches/README.md` for the mechanism and the
arch-difference residual; see `verify.sh` for the automated version of everything below with
RC-propagated pass/fail.

## 0. Pinned source

```
KERNEL_VERSION=6.18.35
KERNEL_URL=https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.18.35.tar.xz
KERNEL_SHA256=f78602932219125e211c5f5bfd84edcfd4ec5ce88fc944f8248413f665bef236
```

Same pin as `guest/linux/versions.lock` (`KERNEL_SHA256`) — this is the same canonical release
line the x86 determinism patches (`consonance/vmm-backend/kvm-patches/`) target, so the two
series describe the same base tree.

```sh
curl -fsSL -o linux-6.18.35.tar.xz "$KERNEL_URL"
echo "$KERNEL_SHA256  linux-6.18.35.tar.xz" | sha256sum -c -
```

## 1. Fresh checkout + tag it

```sh
rm -rf linux-6.18.35 && tar xf linux-6.18.35.tar.xz
cd linux-6.18.35
git init -q
git add -A
git -c user.name=spike -c user.email=s@s commit -q -m 'v6.18.35 pristine'
git tag v6.18.35-pristine
```

(This is exactly how the build container's `/work/linux-6.18.35` was prepared — a git repo
rooted at the pristine extracted tarball, tagged `v6.18.35-pristine`, so the patch can be
produced and re-applied with `git am`/`git format-patch` against a real commit graph.)

## 2. Apply the patch

```sh
git am /path/to/spikes/arm-altra/host/patches/0001-KVM-arm64-add-KVM_EXIT_PREEMPT-in-kernel-force-exit-.patch
```

`git am` applies clean to pristine `v6.18.35-pristine` — this is acceptance gate #1
(reproduced by `verify.sh`). If it does not apply cleanly against a *different* checkout of the
same tag, that's a real problem worth reporting, not something to route around with `--3way` or
`-C1` fuzz — the patch is generated directly from this exact tree.

## 3. Configure

```sh
make ARCH=arm64 defconfig
scripts/config -e VIRTUALIZATION -e KVM -d DEBUG_INFO_BTF -d DEBUG_INFO
make ARCH=arm64 olddefconfig
```

**Caveat found while running this recipe for real:** `-d DEBUG_INFO` alone does not fully turn
debug info off in this tree. `CONFIG_DEBUG_INFO` is a *derived* symbol (`lib/Kconfig.debug`, a
bare `bool` with no user prompt) that gets re-selected by whichever member of the "Debug
information" `choice` group is active — this defconfig's default member is
`DEBUG_INFO_DWARF_TOOLCHAIN_DEFAULT`, which `select`s `DEBUG_INFO`. `scripts/config -d
DEBUG_INFO` edits the `.config` text but `make olddefconfig` recomputes `DEBUG_INFO=y` right back
from the still-active choice member. The `-d DEBUG_INFO_BTF` half of the command *does* work
(it's a plain bool, no choice group) and is the one that actually matters here — it removes the
`pahole` dependency, and this build container has no `pahole` installed. If a genuinely leaner,
faster build is wanted (recommended before a `vmlinux` link — see §5), explicitly flip the
choice instead:

```sh
scripts/config -d DEBUG_INFO_DWARF_TOOLCHAIN_DEFAULT -e DEBUG_INFO_NONE
make ARCH=arm64 olddefconfig
```

Verify with `grep -E '^CONFIG_DEBUG_INFO(_NONE)?=' .config` — a clean disable shows only
`CONFIG_DEBUG_INFO_NONE=y` and no `CONFIG_DEBUG_INFO=y` line at all.

`CONFIG_KVM` ends up `=y` (**built-in**, not `=m`) on this arm64 defconfig — see the arrival-day
note in §5 below; this is different from x86, where the recipe builds `kvm.ko`/`kvm-intel.ko` as
loadable modules.

## 4. Build

```sh
make ARCH=arm64 -j"$(nproc)" arch/arm64/kvm/
```

Recorded build (this container, 10 vCPUs, gcc 14.2.0 Debian trixie, native aarch64):

```
arch/arm64/kvm/built-in.a   (27 objects on the pre-existing baseline; the same count plus the
                              4 patched files rebuilding — arm.o, handle_exit.o,
                              plus the .h-dependent recompiles — every time the patch is applied)
```

Clean: no new warnings, no errors, verified by grepping the full build log for
`warning:`/`error:` and finding nothing beyond the expected zero. This is gate #2, reproduced by
`verify.sh`, which also asserts (by disassembling the built `.o` files, not by reading source)
that the patched exit path is actually present in the compiled objects — see `verify.sh`'s
`assert_compiled_in` step and the specific instruction sequences it checks for.

## 5. Arrival-day fact: arm64 KVM is built-in, not a module

**Unlike the x86 recipe** (`consonance/vmm-backend/kvm-patches/BUILD.md`), which produces
`kvm.ko` + `kvm-intel.ko` and hot-swaps them into a *running* kernel with `rmmod`/`insmod` — no
reboot needed — **this arm64 defconfig builds `CONFIG_KVM=y`, compiled directly into `vmlinux`.
There is no `kvm.ko` to hot-swap.** The only way to run guests under this patch is to **boot the
patched host kernel itself** (a new `vmlinux`/`Image`, a new bootloader entry, a reboot of the
Altra box). This is a meaningful operational difference stage AA-3 inherits: there is no
"load the patched module, keep the box's stock kernel booted, test, unload, revert" live-update
workflow on arm64 the way there is on the x86 determinism box. Every patched-kernel test cycle
on Altra costs a reboot. (It is possible to reconfigure `CONFIG_KVM=m` instead — arm64 does
support KVM as a module — but that is a config change from the defconfig baseline this draft
verifies against, not something this patch or its build recipe assumes; note it here as an
option for AA-3 to evaluate if reboot-per-cycle proves too expensive.)

## 6. The heavier proof: full `vmlinux` link

`arch/arm64/kvm/` compiling clean proves the patch is internally consistent, but an
unreferenced-symbol or UAPI-numbering mistake (e.g. a duplicate exit-reason value, a struct
layout change that some other translation unit assumed differently) only shows up at
whole-kernel link time. Run once:

```sh
make ARCH=arm64 -j"$(nproc)" vmlinux
```

This is slow (a full defconfig `vmlinux` build, not just the KVM subtree) — expect it to take
several times longer than the `arch/arm64/kvm/` build alone. It was run for this deliverable;
see the top-level report for whether it completed within budget and its result.

## What proves the gate

1. `git am` exits 0 against a fresh `v6.18.35-pristine` checkout.
2. `make ARCH=arm64 -j$(nproc) arch/arm64/kvm/` exits 0 with an empty
   `warning:`/`error:` grep over its full log.
3. The compiled `.o` files contain the patched code paths (checked by disassembly, not by
   re-reading the source — see `verify.sh`).
4. `make ARCH=arm64 -j$(nproc) vmlinux` exits 0 (heavier, run once, see the report for whether
   it completed and its outcome).

None of the above is a claim about runtime behavior on real hardware. "Applies + compiles" is
the entire claim this draft makes.
