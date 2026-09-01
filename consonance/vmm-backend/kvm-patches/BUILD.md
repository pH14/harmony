# Build the deterministic-intercept KVM patch series

Apply the three patches to a fresh checkout of the pinned Linux tag and build
KVM as modules:

```sh
git clone --depth 1 --branch v6.18.35 \
  https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git linux-6.18.35
cd linux-6.18.35
git am /path/to/harmony/consonance/vmm-backend/kvm-patches/patches/0001-*.patch \
       /path/to/harmony/consonance/vmm-backend/kvm-patches/patches/0002-*.patch \
       /path/to/harmony/consonance/vmm-backend/kvm-patches/patches/0003-*.patch
make defconfig
scripts/config -e VIRTUALIZATION -m KVM -m KVM_INTEL \
  -d DEBUG_INFO_BTF -d DEBUG_INFO -d MODULE_SIG -d MODULE_SIG_ALL
make olddefconfig
make -j16
```

The resulting modules have the built kernel's vermagic and can only be loaded
into a matching kernel. Loading replacement KVM modules disrupts running VMs;
perform live validation only on a dedicated host and restore the distribution
modules afterward.
