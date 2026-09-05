<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# play-agent

The play-agent is a headless Linux guest frontend for the NES workloads. Its
portable library contains the input alphabet, SMB RAM-map decoding, billboard
layout, state-register catalog, startup walk, and per-frame loop. The binary
adds the Linux edges: libretro FFI, the `/dev/harmony` SDK transport, and the
guest-physical billboard mapping.

The agent supports the SMB dynamic FCEUmm core and the Nova static QuickNES
mode. It disables audio and video during search, advances by the emulator's
frame counter, and supplies controller chords from the seeded SDK entropy
stream. At each frame boundary it publishes the billboard and state registers;
the host interprets those events and owns snapshots.

Run the portable bring-up smoke without a ROM or hypervisor:

```sh
cargo run --manifest-path consonance/harmony-linux/play-agent/Cargo.toml -- --smoke
```

The image builder supplies the core and ROM paths. The real binary is Linux
guest code; the library and mock-core tests run on the development host.
`static-quicknes` is available for images whose userspace cannot execute the
dynamic loader before the guest clock is enabled.
