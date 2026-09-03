<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# harmony-sdk

`harmony-sdk` is a `no_std`, allocation-free guest SDK generic over a
`hypercall_proto::Transport`. It provides catalog declarations, assertions,
state registers, buggify decisions, lifecycle points, coverage-yield handshakes,
and seeded entropy through the existing hypercall services.

The SDK is hooks and transport only. It emits raw event identities and values;
the host supplies timestamps, interprets `state_max`, resolves buggify, and
turns lifecycle events into snapshot boundaries. Event IDs use an 8-bit
namespace and 24-bit local identifier. The wire constants and payload builders
live in `src/wire.rs`.

`init` sends one catalog frame, so a catalog must fit in one event payload. Point
coordinates and names must be unique and local IDs must fit the 24-bit wire
field. The standalone workspace can be checked with:

```sh
cargo test --manifest-path consonance/harmony-linux/sdk/Cargo.toml
cargo build --manifest-path consonance/harmony-linux/sdk/Cargo.toml \
  --lib --target x86_64-unknown-none
```
