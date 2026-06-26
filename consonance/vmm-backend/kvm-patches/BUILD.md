# BUILD.md — apply → build → load → revert for the deterministic-intercept patch

Two builds are documented:

- **Part 1 — canonical, against the pinned `linux-6.18.35` tag.** This is the
  deliverable a reviewer re-applies and re-builds (acceptance gate #2). It
  produces `kvm.ko`/`kvm-intel.ko` with vermagic `6.18.35-…`.
- **Part 2 — loadable, against the box's running kernel (6.12.90).** The
  contract pins 6.18.35 but the determinism box runs `6.12.90+deb13.1-amd64`; an
  out-of-tree module must match the **running** kernel's vermagic to load, so
  the live round-trip validation uses a 6.12.90 build of the *same* change — a
  deliberate, named version-match proxy (the canonical 6.18.35 build is verified
  but not loadable on the 6.12.90 box). See `IMPLEMENTATION.md`.

Everything below was run on the box (`ssh <det-box>`, root). The box has no
Secure Boot / kernel lockdown (`/sys/kernel/security/lockdown = [none]`,
`module.sig_enforce = N`), so unsigned self-built modules load.

---

## Part 1 — build against the pinned tag `linux-6.18.35` (gate #2)

```sh
# 1. Fetch + verify the pinned source (sha256 matches docs/CPU-MSR-CONTRACT.md
#    and guest/linux/versions.lock).
cd ~/kvm-spike
curl -fsSL -o linux-6.18.35.tar.xz \
  https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.18.35.tar.xz
echo 'f78602932219125e211c5f5bfd84edcfd4ec5ce88fc944f8248413f665bef236  linux-6.18.35.tar.xz' \
  | sha256sum -c -

# 2. Fresh checkout of the tag + apply the series with git am.
rm -rf linux-6.18.35 && tar xf linux-6.18.35.tar.xz
cd linux-6.18.35
git init -q && git add -A && git commit -q -m 'v6.18.35 pristine'
git am /path/to/consonance/vmm-backend/kvm-patches/patches/0001-*.patch \
       /path/to/consonance/vmm-backend/kvm-patches/patches/0002-*.patch \
       /path/to/consonance/vmm-backend/kvm-patches/patches/0003-*.patch

# 3. Configure: KVM + KVM_INTEL as modules; BTF/DEBUG_INFO off (no pahole needed),
#    module signing off (we load unsigned).
make defconfig
scripts/config -e VIRTUALIZATION -m KVM -m KVM_INTEL \
  -d DEBUG_INFO_BTF -d DEBUG_INFO -d MODULE_SIG -d MODULE_SIG_ALL -d MODULE_SIG_FORCE
make olddefconfig

# 4. Build. Produces arch/x86/kvm/kvm.ko and arch/x86/kvm/kvm-intel.ko.
make -j"$(nproc)"
modinfo -F vermagic arch/x86/kvm/kvm-intel.ko   # -> 6.18.35-… SMP preempt mod_unload
```

A `make -j16` of `defconfig` takes a few minutes on the determinism box. The
modules' `6.18.35-…` vermagic is **why they will not load into the running
6.12.90 kernel** — use Part 2 for the live experiments.

---

## Part 2 — loadable build for the running 6.12.90 kernel (live experiments)

The same three-hunk change is applied to the running kernel's KVM source and
built against the distro's prepared header tree (so vermagic + the
`CONFIG_MODVERSIONS` symbol CRCs match `uname -r` exactly).

This is the exact procedure run for the live experiments (the same change ported
to 6.12; the only code delta is `EXPORT_SYMBOL_FOR_KVM_INTERNAL` →
`EXPORT_SYMBOL_GPL`, which is what `scripts/apply_patch_612.py` + the sed below
handle).

```sh
cd ~/kvm-spike/deb612

# 1. The running kernel's source (Debian applies its patch series on unpack)
#    and the matching header/kbuild .debs (extracted into the home dir — no
#    system install needed; all are downloads, not `apt-get install`).
apt-get source linux=6.12.90-2
for p in linux-headers-6.12.90+deb13.1-amd64 \
         linux-headers-6.12.90+deb13.1-common \
         linux-kbuild-6.12.90+deb13.1; do apt-get download "$p"; done
mkdir -p hdr && for d in *.deb; do dpkg -x "$d" hdr; done
B=$PWD/hdr/usr/src/linux-headers-6.12.90+deb13.1-amd64        # objtree: .config, Module.symvers, utsrelease
CM=$PWD/hdr/usr/src/linux-headers-6.12.90+deb13.1-common      # srctree

# 2. Apply the same change to the Debian source, then swap the export macro
#    (the 6.18 namespaced macro does not exist in 6.12).
cd linux-6.12.90
python3 /path/to/consonance/vmm-backend/kvm-patches/scripts/apply_patch_612.py
sed -i 's/EXPORT_SYMBOL_FOR_KVM_INTERNAL/EXPORT_SYMBOL_GPL/g' arch/x86/kvm/x86.c
cd ..

# 3. Overlay patched sources + headers into the (srctree) build tree. KVM also
#    pulls in virt/kvm, and the headers package ships only headers, so the .c
#    files and the two patched headers must be copied in.
cp -r linux-6.12.90/arch/x86/kvm "$CM/arch/x86/"
cp -r linux-6.12.90/virt "$CM/"
cp linux-6.12.90/include/uapi/linux/kvm.h          "$CM/include/uapi/linux/kvm.h"
cp linux-6.12.90/arch/x86/include/asm/kvm_host.h   "$CM/arch/x86/include/asm/kvm_host.h"

# 4. Point the build tree's absolute common-include at the home-extracted common,
#    and disable module BTF (Debian config enables it; pahole is not installed).
sed -i "s#/usr/src/linux-headers-6.12.90+deb13.1-common#$CM#g" "$B/Makefile"
sed -i "/^CONFIG_DEBUG_INFO_BTF_MODULES=y/d" "$B/include/config/auto.conf"

# 5. Build the KVM modules against the header tree (vermagic + symbol CRCs match).
make -C "$B" M=arch/x86/kvm modules -j"$(nproc)"
modinfo -F vermagic "$B/arch/x86/kvm/kvm-intel.ko"   # -> 6.12.90+deb13.1-amd64 SMP preempt mod_unload modversions
```

The patched modules land in `$B/arch/x86/kvm/{kvm,kvm-intel}.ko`. Use those paths
in the load step below.

### Load (hot-swap) — PRIVILEGED, modifies the live kernel

> No other KVM workload may be active during the window (the CI runner runs
> cargo gates, not VMs — confirm `lsof /dev/kvm` is empty and `lsmod` shows
> `kvm_intel … 0` users). Swap order matters: `kvm_intel` depends on `kvm`.

```sh
B=~/kvm-spike/deb612/hdr/usr/src/linux-headers-6.12.90+deb13.1-amd64/arch/x86/kvm
rmmod kvm_intel kvm                 # unload stock pair
insmod "$B/kvm.ko"                  # load patched core first
insmod "$B/kvm-intel.ko"            # then the Intel vendor module
dmesg | tail -5                     # expect clean load, no taint beyond out-of-tree
ls -l /dev/kvm                      # recreated
```

### Validate (with the patched modules loaded)

With the patched pair loaded, the in-tree box gates exercise the determinism
intercept end to end. Run them on the determinism box, CPU-pinned (see
`docs/BOX-PINNING.md`):

```sh
ssh <det-box> 'taskset -c 1 cargo test -p vmm-core --test live_determinism -- --ignored --nocapture'
```

`live_determinism` (and `box_corpus`) require these patched modules loaded; they
are the tracked replacement for the spike's original throwaway measurement harness.

### Revert (TESTED — leave the box on stock KVM)

```sh
rmmod kvm_intel kvm                 # unload patched pair
modprobe kvm_intel                  # pulls stock kvm + kvm_intel back in
ls -l /dev/kvm                      # recreated by the stock modules
modinfo -F vermagic $(modinfo -n kvm_intel)    # stock path under /lib/modules
```

If `rmmod` reports the module is in use, stop every VM first (`fuser -k
/dev/kvm` as a last resort) and retry. If a patched module ever wedges KVM such
that `rmmod` fails, a **reboot** restores the stock signed modules from
`/lib/modules/$(uname -r)` cleanly (the patched modules are never installed
there — they are only ever `insmod`-ed from the build dir).

**The spike must end with the box on stock KVM.** Never leave a patched module
loaded.
