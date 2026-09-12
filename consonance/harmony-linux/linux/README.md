<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Harmony Linux platform artifacts

This directory owns the pinned Linux kernels, direct platform fixtures, and
the workload-free OCI runtime. Sources come from `versions.lock`; the build
applies the common kernel series followed by the target architecture series,
merges the fixed configuration, and publishes hash-checked artifacts.

The standard OCI runtime contains one kernel configuration per architecture,
the pinned static `runc`, static BusyBox utilities, `/init`, and the
platform-owned PID 1 and supervisor supplied through build inputs. It does not
select or build an application image. The platform init receives the prepared
bundle and owns guest setup, device exposure, runtime invocation, and terminal
lifecycle.

## Canonical artifacts

The x86 build publishes `build/x86_64/bzImage` and
`build/x86_64/initramfs-oci.cpio.gz`. The native arm64 build publishes
`build/aarch64/Image` and `build/aarch64/initramfs-oci.cpio.gz`. Each runtime
directory also contains a checksum and `oci-runtime.manifest` describing the
kernel, initramfs, BusyBox, `runc`, PID 1, and supervisor inputs.

Build the complete runtime from the repository root on native Linux:

```sh
make -C consonance/harmony-linux fetch
consonance/harmony-linux/scripts/build-platform-runtime.sh
```

The build requires `nightly-2026-06-16` with `rust-src` and the native Linux
musl target. It builds the matching kernel and static guest binaries, packages
the platform OCI fixture, and records a source and artifact manifest. The
lower-level `build-oci-runtime-initramfs.sh` accepts the built platform init and
supervisor through `HARMONY_RUNTIME_INIT` and `HARMONY_RUNTIME_SUPERVISOR`.
It fails when a required input is missing. Every ARM executable must satisfy
the existing LSE and counter reachability gates.

## Direct platform fixtures

These targets remain direct substrate checks and are independent of the OCI
runtime assembly:

```sh
make -C consonance/harmony-linux/linux image
make -C consonance/harmony-linux/linux test
make -C consonance/harmony-linux/linux arm64-image
make -C consonance/harmony-linux/linux exec-image
make -C consonance/harmony-linux/linux go-runtime-image
```

The x86 kernel's default, traps-off, and faultlab outputs are separate test
artifacts with their own instruction audit baselines. The arm64 traps-off
output is likewise a deliberate negative control. These controls do not alter
the standard OCI runtime configuration.

Workload image recipes and their pins live under `workloads/guest-images` and
own their fetch entrypoint. Other workload packages own their inputs in the
same way. The platform fetch entrypoint downloads only the kernel, BusyBox,
arm64 musl source, and static `runc` assets.

The reproducibility manifest records the patch series, configuration inputs,
and generated artifact hashes. Build transcripts are evidence, not inputs.
