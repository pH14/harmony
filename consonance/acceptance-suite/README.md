<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Acceptance suite

`acceptance-suite` is Consonance's executable acceptance surface. It combines
the O1/O2/O3 oracle runner with small bare-metal workloads and the reviewed
observations produced by those workloads.

The Rust crate parses and validates a corpus manifest, runs selected oracles,
and emits machine-readable reports. The standalone `payloads/` workspace builds
Multiboot-v1 `x86_64-unknown-none` images. `golden/` contains serial-shape
files and hardware-gate observation digests.

## Entry points

```sh
cargo run -p acceptance-suite -- validate --manifest <manifest>
cargo run -p acceptance-suite -- run --manifest <manifest>
make -C consonance/acceptance-suite test-payloads
```

The payload gate runs every image twice under QEMU TCG and compares its payload
output byte-for-byte. The hardware-backed corpus gate is driven from the VMM
test harness and uses the same payloads, manifest, and goldens.

This directory owns acceptance workloads and oracle plumbing. Linux kernels,
guest agents, and compatibility libraries live under
`consonance/harmony-linux`.
