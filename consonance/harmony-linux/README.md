<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# harmony-linux

`harmony-linux` contains the guest-side environment for consonance: pinned
Linux sources and image builders, the `/dev/harmony` integration, guest agents,
and the no-std SDK used by those agents. Bare-metal acceptance payloads and
instruction audit tables live alongside the scripts in this component.

## Entry points

```sh
make -C consonance/harmony-linux fetch
make -C consonance/harmony-linux test-libvoidstar
make -C consonance/harmony-linux test-linux
make -C consonance/harmony-linux test
```

`fetch` downloads and verifies the pinned platform kernel, generic userland,
and static OCI runtime sources. Workload packages own their separate fetch
entrypoints.
`test-libvoidstar` runs the portable ABI and device-transaction checks.
`test-linux` builds the Linux artifacts twice and runs the image gate; it
requires Linux, or a Linux/amd64 build container on macOS. Build output lives
in `consonance/harmony-linux/build/`; `GUEST_BUILD_ROOT` can select another
build root.

The root `flake.nix` provides the locked release-image entry point on native
Linux:

```sh
nix run .#guest-images -- --output "$PWD/guest-output"
```

On Linux/x86_64 that produces the standard platform kernel, direct fixture
initramfs, and (when the platform runtime inputs are supplied) the canonical
`x86_64/initramfs-oci.cpio.gz`; the emitted `MANIFEST.sha256` covers every
staged artifact. Native Linux/aarch64 produces the corresponding `aarch64`
directory.

The pinned BusyBox source is also available as a standalone flake package for
reproducible image preparation and CI reuse:

```sh
nix build .#busybox-source --no-link --print-out-paths
```

The resulting store path is the hash-verified `busybox-1.38.0.tar.bz2` source
from the same pin used by the guest image builder. Workload acceptance fetches
that archive from Buildroot's mirror, with the Nix package as a fallback, and
checks the same lock-file SHA-256 before building. An upstream download outage
therefore does not change the accepted source bytes.

## Components

- `linux/` builds the pinned kernels, direct platform fixtures, and the
  workload-free OCI runtime. Kernel patches provide the guest device,
  paravirtual clock, observation, and task-park interfaces; build scripts
  verify source and artifact hashes.
- `libvoidstar/` implements the SDK-facing dynamic ABI and communicates with
  `/dev/harmony`.
- `sdk/` provides the no-std event, state, assertion, lifecycle, and entropy
  hooks used by guest payloads.
- Workload packages under `workloads/` build their own images and fetch their
  own application inputs after the platform artifacts are prepared.

The guest transport is synchronous and serialized by the kernel driver. Guest
entropy comes from the host-provided seeded service; the compatibility library
does not provide a host-randomness fallback.

Workload image recipes that use GNU cpio require version 2.14 or newer so
`--reproducible` normalizes inode, device, and directory-link metadata before
an image hash is recorded.

Cold Nix guest builds fetch the pinned BusyBox archive from the Buildroot mirror
with the upstream URL as fallback. Both locations use the same locked SHA-256;
the mirror choice leaves the guest source version and bytes unchanged.
