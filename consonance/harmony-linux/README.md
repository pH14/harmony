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

On Linux/x86_64 that produces `x86_64/bzImage`, `x86_64/bzImage-faultlab`, the
minimal initramfs, and `x86_64/initramfs-go-runtime.cpio.gz`; the emitted
`MANIFEST.sha256` covers every staged artifact.

The pinned BusyBox source is also available as a standalone flake package for
reproducible image preparation and CI reuse:

```sh
nix build .#busybox-source --no-link --print-out-paths
```

The resulting store path is the hash-verified `busybox-1.38.0.tar.bz2` source
from the same pin used by the guest image builder. NES acceptance fetches that
archive from Buildroot's mirror, with the Nix package as a fallback, and checks
the same lock-file SHA-256 before building. An upstream download outage therefore
does not change the accepted source bytes.

## Components

- `linux/` builds the pinned kernel, its x86 profiles, and workload-specific
  initramfs images. Kernel patches provide the guest device, paravirtual clock,
  and task-park interfaces; build scripts verify source and artifact hashes.
- `libvoidstar/` implements the SDK-facing dynamic ABI and communicates with
  `/dev/harmony`.
- `sdk/` provides the no-std event, state, assertion, lifecycle, and entropy
  hooks used by guest payloads.
- `workloads/nes-guest/` builds the headless NES workload and publishes its
  state through the SDK. The historical `linux/build-*-game-image.sh` entry
  points remain as compatibility launchers for the package-owned recipes.

The guest transport is synchronous and serialized by the kernel driver. Guest
entropy comes from the host-provided seeded service; the compatibility library
does not provide a host-randomness fallback.

The x86 Nova image requires GNU cpio 2.14 or newer. Its `--reproducible`
mode normalizes inode, device, and directory-link metadata before the
initramfs hash is recorded in deterministic campaign streams.

Cold Nix guest builds fetch the pinned BusyBox archive from the Buildroot mirror
with the upstream URL as fallback. Both locations use the same locked SHA-256;
the mirror choice leaves the guest source version and bytes unchanged.

## XSAVE consistency qualification in progress

The September 14 workstream keeps AVX and targets deterministic guest-owned
XSAVE buffers on hosted AMD as well as Intel. The broad snapshot integration
goal remains open; this work does not qualify arbitrary supplied workloads.

Qualification proceeds through a traced fixed-CPU reuse cohort (D1), a
bare-metal AMD attribution run (D2), synthetic whole-buffer canonicalization
with init/SSE-active/AVX-active states (D3), and an inventory of kernel save
sites, XGETBV(1), signal-frame handling, and userspace loader behavior (D4).
Nested-page-fault interference in the reuse fixture is a hypothesis until the
exit trace establishes it. No guest canonicalizer is shipped before D3 and D4.

The proposed guest contract compares identity at guest-initiated exits; debug
stops are interventions, not comparison points. Patched save/canonicalize
sequences must exclude guest interrupt handlers. Admitted executable code,
including shared libraries and dlopen dependencies, must exclude XSAVE-family
instructions and XGETBV(1) outside the patched kernel. XGETBV(0) needs a proven
zero selector. JITs, generated code, and writable executable mappings are
outside that controlled admission scope. These are requirements to implement
and verify, not claims about the current image or scan.

The primary path adapts whole-buffer canonicalization to each audited save
layout and fault path, disables modified-save optimizations, and uses eager
dynamic binding in admitted workloads. Disabling XSAVE/AVX is a fallback only
if a correct synthetic canonicalizer exposes actual register-value corruption.
Raw restore presence stays in the restore path and diagnostics. Removing it
from logical identity requires the kernel audit and admission argument, not
merely matching boot tests. Real-Linux paired boots and interrupted runs must
then agree in complete RAM and modeled state on every qualification host.
