<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# harmony-linux

`harmony-linux` contains the guest-side environment for consonance: pinned
Linux sources and image builders, the `/dev/harmony` integration, guest agents,
and the no-std SDK used by those agents. Bare-metal acceptance payloads and
their goldens live in `consonance/acceptance-suite`.

## Entry points

```sh
make -C consonance/harmony-linux fetch
make -C consonance/harmony-linux test-libvoidstar
make -C consonance/harmony-linux test-linux
make -C consonance/harmony-linux test
```

`fetch` downloads and verifies the pinned kernel and userland sources.
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

## Components

- `linux/` builds the pinned kernel and workload-specific initramfs images.
  Kernel patches provide the guest device and paravirtual clock interfaces;
  build scripts verify source and artifact hashes.
- `libvoidstar/` implements the SDK-facing dynamic ABI and communicates with
  `/dev/harmony`.
- `sdk/` provides the no-std event, state, assertion, lifecycle, and entropy
  hooks used by guest payloads.
- `play-agent/` runs the headless NES workload and publishes its state through
  the SDK. `tetanes-agent/` is the arm64 TetaNES payload.

The guest transport is synchronous and serialized by the kernel driver. Guest
entropy comes from the host-provided seeded service; the compatibility library
does not provide a host-randomness fallback.

The x86 Nova image requires GNU cpio 2.14 or newer. Its `--reproducible`
mode normalizes inode, device, and directory-link metadata before the
initramfs hash is recorded in deterministic campaign streams.
