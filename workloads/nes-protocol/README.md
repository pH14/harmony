# NES publication protocol

Shared guest writers and host readers for versioned NES memory publications.
Version 1 carries emulator state and work RAM. Version 2 carries a bounded
per-action work-RAM ring, save RAM, and an optional emulator-state region.

The host decodes at a stopped guest action boundary. Fields are little-endian;
readers validate versions, flags, bounds, and frame counts. Golden header fixtures
and malformed-publication tests preserve the contract independently of either
execution backend.

```sh
cargo test --manifest-path workloads/nes-protocol/Cargo.toml
```
