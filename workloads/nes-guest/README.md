<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# play-agent

The play-agent is a headless Linux guest frontend for the NES workloads. Its
portable library contains the input alphabet, SMB RAM-map decoding, billboard
layout, state-register catalog, startup walk, and per-frame loop. The binary
adds the Linux edges: libretro FFI, the `/dev/harmony` SDK transport, and the
guest-physical billboard mapping.

The agent supports the SMB dynamic FCEUmm core, the Nova static QuickNES mode,
and the game-neutral NES payload mode. It disables audio and video during
search, advances by the emulator's frame counter, and supplies controller
chords from the seeded SDK entropy stream. At each frame boundary it publishes
the billboard and state registers; the host interprets those events and owns
snapshots.

Run the portable bring-up smoke without a ROM or hypervisor:

```sh
cargo run --manifest-path workloads/nes-guest/Cargo.toml -- --smoke
```

The image builder supplies the core and ROM paths. The real binary is Linux
guest code; the library and mock-core tests run on the development host.
`static-quicknes` is available for images whose userspace cannot execute the
dynamic loader before the guest clock is enabled.

## Generic NES base image

`build-base-image.sh` builds `initramfs-nes.cpio.gz`, a ROM-free base image
containing BusyBox and a static QuickNES `play-agent`. It uses the pinned Linux
image helpers in `consonance/harmony-linux/linux/lib-build.sh` and must run on
the native Linux host for its architecture. Supply a prebuilt archive named
`libquicknes_libretro.a` unless `PLAY_AGENT_BIN` points at an already-built
static agent:

```sh
HARMONY_QUICKNES_STATIC_LIB=/path/to/libquicknes_libretro.a \
  ./workloads/nes-guest/build-base-image.sh
```

The artifact contains no ROM and no boot entrypoint. The workload package's
`prepare(rom, base_initramfs)` appends a deterministic cpio overlay containing
`/game.nes` and `/init`; that entrypoint runs:

```text
/opt/harmony/play-agent --nes-payload --rom /game.nes
```

On Linux x86_64 the guest SDK uses
`hypercall_doorbell::linux::DeviceTransport` over the kernel-owned
`/dev/harmony` device. The shared adapter is enabled by the `linux-device`
feature and keeps ioctl pointer and frame bounds in one tested implementation.
arm64 continues to use the existing board MMIO doorbell and fixed pages.
