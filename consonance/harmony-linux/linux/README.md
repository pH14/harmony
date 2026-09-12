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

Cgroup v2 device enforcement requires `BPF_SYSCALL` and `CGROUP_BPF` for
`runc`. Both architectures use the BPF interpreter with `BPF_JIT` disabled,
so cgroup policy does not introduce dynamically generated kernel instructions.
`FHANDLE` is disabled: `name_to_handle_at` and `open_by_handle_at` are outside
the supported execution contract. This keeps privileged payloads inside the
delegated cgroup view rather than exposing its device-policy ancestor through
filesystem or namespace handles.

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

The arm64 runtime uses `build-arm64-runc.sh` to build runc 1.5.0 from the
source and Go bootstrap pins in `versions.lock`. The script exports UAPI
headers from the pinned kernel, builds a fresh LSE-only musl toolchain, applies
the two Go runtime patches under `patches/go`, and publishes the scan-checked
binary as `build/aarch64/runc`. Its vendored build uses Go 1.25.0 locally with
`netgo`, `osusergo`, and `urfave_cli_no_docs`; `seccomp` and `libpathrs` are
omitted because their native ARM dependencies are not part of the platform
closure, and the generated runtime requests neither feature.

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
arm64 musl source, the runc source, and the Go arm64 bootstrap archive.

The reproducibility manifest records the patch series, configuration inputs,
and generated artifact hashes. Build transcripts are evidence, not inputs.
