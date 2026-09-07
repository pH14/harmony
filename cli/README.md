<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Harmony CLI

Build from the repository with `cargo build --release -p harmony-cli`.
Run `target/release/harmony preflight` to inspect host support and guest artifacts.
See the [harmony-linux README](../consonance/harmony-linux/README.md) for guest image builds. Set `HARMONY_GUEST_DIR`
to the artifact directory when using an external build.

## Search packages

```sh
harmony search --package nes smb.nes --core quicknes_libretro.so
harmony search --package nes --backend native smb.nes --core quicknes_libretro.so
harmony search --package nes --backend consonance smb.nes \
  --kernel bzImage --base-initramfs initramfs-nes.cpio.gz
harmony search --package faults foo.oci
```

NES identifies SMB or Nova by ROM hash and defaults to `native`. Supply the
pinned host QuickNES library with `--core` or `HARMONY_QUICKNES_CORE`. Consonance
execution uses a controlled kernel and the ROM-free image produced by
[`build-base-image.sh`](../workloads/nes-guest/build-base-image.sh); preparation
adds the ROM and launch command. It requires a supported Linux KVM host.

The faults package defaults to `consonance`. Its OCI image supplies
`/harmony/workload.json`, node executables, topology setup, and check/recovery
commands using the [workload schema](../workloads/faults/src/spec.rs).
All replicas and the fault supervisor execute inside one VM on one virtual CPU.
Supply the controlled kernel with `--kernel`, the Linux base image with
`--base-initramfs`, and a static `fault-guest` binary with `--fault-agent` or
`HARMONY_FAULT_AGENT`. Installed artifacts are discovered through
`HARMONY_GUEST_DIR`. Preparation injects the supervisor into the staged image;
its commands execute inside the guest.

`--seed`, `--workers`, `--executions`, and `--actions` bound the campaign's logical
work. `--out` selects a fresh output directory. `prepared.json` records the
resolved workload, backend artifacts, and search settings; `stream.jsonl`,
`checkpoint.json`, and `report.json` retain campaign choices, state, and results.
An explicit backend selection is checked before execution.

## OCI execution

```
harmony oci run alpine:3 --seed 7 --timeout 60 --out run-7 -- /bin/echo hello
```

`oci run` accepts a registry image, OCI layout, or Docker image archive. It writes
`serial.log` and `run.json` on completion. `--console` streams the full boot log.
On timeout it preserves the partial serial log and returns an error without a
successful run digest.

On Linux x86, the timeout watchdog sets a host cancellation latch and interrupts
the owning KVM thread with reserved SIGUSR1. It repeats the interrupt after expiry
until the driver returns, covering a signal arriving just before KVM_RUN. It sends
no signals before expiry; canceled executions are abandoned. The timeout is a host
resource limit, not guest virtual time or replay state. The mechanism itself lives
in [`consonance-client`](../consonance/client/README.md), which the neutral session
also uses for its own host bound.

The CLI enables `harmony_pvclock` so the kernel uses virtual timing for entropy
mixing as well as timekeeping. The stock x86 virtual-time boot supplies Linux's `SETUP_RNG_SEED` record from the
VM's seeded entropy stream. This makes the CRNG ready without waiting for timing
jitter that cannot advance inside a non-exiting guest loop. The boot consumes 64
bytes from that same stream before guest execution. Hardware RNG instructions stay
hidden. The pinned Linux kernel must trust bootloader randomness (its default);
`random.trust_bootloader=off` disables this readiness mechanism.

The OCI CI gate reads `/dev/urandom` and checks byte-identical serial logs and
digests for repeated seeds, distinct output for different seeds, and cancellation
of a guest loop that performs no I/O.
