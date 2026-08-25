# The stock kernel was upgraded out from under the sealed pack, and put back

Stage 0 runs 1 and 2 sealed two KVM module identities read on Debian's
`6.12.95+deb13-amd64`. At the close, `/boot` no longer had that kernel.

`/var/log/apt/history.log` says what happened:

```
Install: linux-image-6.12.101+deb13-amd64:amd64 (6.12.101-1, automatic)
Upgrade: linux-image-amd64:amd64 (6.12.95-1, 6.12.101-1)
Remove:  linux-image-6.12.95+deb13-amd64:amd64 (6.12.95-1)
```

Installing the kernel build dependencies for item 6 pulled the `linux-image-amd64`
meta-package forward, which installed 6.12.101 and removed 6.12.95's image. Only an empty
module directory was left behind. The running kernel was unaffected at the time, so
nothing in stages 0 or 1 was measured on a different kernel than the one they recorded.

Booting 6.12.101 instead would have failed stage 0 on both `kvm-module-identity` rows,
because the modules of a different kernel build have different build-ids. That is the
pack working as intended: it pins the hypervisor by content, and the content changed.

`6.12.95-1` is still carried by `trixie-security`, so it was reinstalled and the GRUB
default pinned to it by name. The close then booted the kernel the pack seals.

Two things worth carrying forward.

- A qualification box that installs packages during a program can lose the kernel its
  baseline was sealed against, silently, at a moment unrelated to any measurement. Pin
  `linux-image-amd64` or install build dependencies with `--no-install-recommends` and a
  held meta-package.
- The pack pinning module identity by build-id is what turns that into a loud stage-0
  refusal rather than a quiet re-measurement on a different hypervisor.
