# Kernel patch series — `git format-patch` against the `linux-6.18.35` tag

Apply to a fresh checkout of the pinned tag with `git am`:

```sh
git clone --depth 1 --branch v6.18.35 \
  https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git linux-6.18.35
cd linux-6.18.35
git am /path/to/consonance/vmm-backend/kvm-patches/patches/0001-*.patch ...
```

See `../BUILD.md` for the full apply → build → load → revert recipe.
The series is intentionally minimal (3 commits, +203/−2 lines): it enables the
three VMX exiting controls (RDTSC/RDTSCP via PROCBASED bit 12; RDRAND via
PROCBASED2 bit 11; RDSEED via PROCBASED2 bit 16) and routes each VM-exit to
userspace as `KVM_EXIT_DETERMINISM`, with a completion path that writes the
destination register(s) and advances RIP. Opt-in per VM via
`KVM_CAP_X86_DETERMINISTIC_INTERCEPTS` (default-off → stock behavior).

- `0001-KVM-x86-add-KVM_EXIT_DETERMINISM-userspace-exit-ABI.patch`
- `0002-KVM-x86-emulate-intercepted-RDTSC-RDTSCP-RDRAND-RDSE.patch`
- `0003-KVM-VMX-enable-RDTSC-RDRAND-RDSEED-exiting-for-the-d.patch`

Verified: `git am`-clean on a fresh `linux-6.18.35` checkout and the out-of-tree
modules build cleanly. `scripts/apply_patch.py` reproduces the
same edits by string anchor; `scripts/apply_patch_612.py` ports them to the
Debian 6.12.90 source for the loadable proxy build (`../BUILD.md` Part 2).
