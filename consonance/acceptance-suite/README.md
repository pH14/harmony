# `acceptance-suite`

The engine's acceptance product combines the O1/O2/O3 oracle runner with the
portable bare-metal workloads and committed goldens it evaluates:

- `cargo run -p acceptance-suite -- ...` runs or validates a manifest;
- `make -C consonance/acceptance-suite test-payloads` builds every Multiboot
  payload, boots it twice under QEMU TCG, and compares byte-exact output;
- `payloads/` is a standalone `x86_64-unknown-none` Cargo workspace;
- `golden/` contains the reviewed observations and digests.

This directory is consonance's test surface, not a guest OS tier. Linux kernels,
agents, and compatibility libraries live under `harmony-linux`.
