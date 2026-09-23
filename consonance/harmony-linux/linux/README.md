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

The common kernel policy disables `RWSEM_SPIN_ON_OWNER` when either Harmony
virtual clock is compiled in. Its reader-owner optimistic-spin timeout uses
`sched_clock()`, which remains frozen during exit-free spinning. Contended
rwsems use Linux's existing blocking and wakeup path.

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
the existing LSE and counter reachability checks.

Both architectures build runc 1.5.0 from the verified source and Go 1.25.0
pins in `versions.lock`. Internal re-execution preserves `LD_BIND_NOW=1`
before child startup even when the runtime constructs a minimal environment.
Each build tests that command environment and emits `runc-build.manifest`.
The x86 builder, `build-x86-runc.sh`, uses pinned musl 1.2.6 for static linking.

The arm64 runtime uses `build-arm64-runc.sh`. The script exports UAPI
headers from the pinned kernel, builds a fresh LSE-only musl toolchain, applies
the two Go runtime patches under `patches/go`, and publishes the scan-checked
binary as `build/aarch64/runc`. Its vendored build uses Go 1.25.0 locally with
`netgo`, `osusergo`, and `urfave_cli_no_docs`; `seccomp` and `libpathrs` are
omitted because their native ARM dependencies are not part of the platform
closure, and the generated runtime requests neither feature.

## Supported x86 guest lifecycle

The supported workload guest is the shipped 64-bit Linux kernel and its
initramfs. The x86 loader requires the [64-bit Linux boot entry](https://docs.kernel.org/arch/x86/boot.html).
The Linux VMM rejects non-long-mode CPU records at snapshot and state-hash
publication and before snapshot restore. These checks observe boundaries;
they do not trap every guest mode transition. Kernel replacement through either
kexec syscall or kexec handover is disabled, alongside modules, suspend, and
hibernation. The kernel builder
checks the resolved configuration before compiling or publishing an image;
an older cached image does not qualify a changed configuration.

Snapshot continuation relies on Linux's architectural page-table update and
translation-invalidation rules. It does not promise persistence of stale
32-bit PAE translations after software changes the PDPT without the required
synchronization. AMD NPT permits those cached entries to be discarded and
reloaded; the synthetic stale-PDPTR tests remain backend diagnostics for that
limitation. Disabling kexec prevents replacing this kernel through its normal
kernel-loading interfaces; it is not a CPU mode firewall. The 64-bit loader
entry alone does not constrain arbitrary supplied kernels or imported CPU
states to remain in long mode. Compatibility-mode userspace under long-mode
paging is distinct from legacy 32-bit PAE paging. Long mode itself requires
CR4.PAE, so that bit alone is not a legacy-PAE signal.

These constraints define the Linux guest qualification scope, not a claim
that the generic backend's AMD PAE continuation failure is fixed.


## Fixed x86 XSAVE layout

The Harmony kernel patch series requires the guest CPUID contract's standard
832-byte FP/SSE/AVX layout and rejects a different enabled mask or optimized
save features at boot. Kernel saves materialize absent components, normalize
reserved bytes, and clear presence bits only for initialized payloads. MXCSR
is captured independently of the SSE presence bit and remains independent in
ptrace and signal import/export. The existing legacy signal-frame epilog
sets FP/SSE bits only after those components have been materialized.

Save and normalization run with local IRQs disabled. This does not by itself
exclude NMIs or prove equality of every kernel stack byte. Whole-RAM snapshot
qualification still requires the paired Linux endpoint oracle. Present x87
register payload is retained even when its tag is empty; FNINIT after active
x87 use is a separate diagnostic case and must not be treated as padding.

Run the exact patch helper's buffer, mask, MXCSR, and checked-fault model with
`consonance/harmony-linux/linux/test-xsave-canonical.sh`. On native Linux,
compile `xsave-guest-check.c` as a static binary and run it inside the patched
guest to exercise ptrace, signal MXCSR roundtrips, and all eight x87 payload
slots. The ignored `vmm-core` test `g1_kernel_xsave_functional` boots these
artifacts with `G1_KERNEL` and `G1_INITRAMFS`; use a release test build on a
KVM host. These checks supplement the endpoint oracle; they do not replace it.

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

The x86 kernel's default, traps-off, and task-park outputs are separate test
artifacts with their own instruction audit baselines. The arm64 traps-off
output is likewise a deliberate negative control. These controls do not alter
the standard OCI runtime configuration.

Workload image recipes and their pins live under `workloads/guest-images` and
own their fetch entrypoint. Other workload packages own their inputs in the
same way. The platform fetch entrypoint downloads only the kernel, BusyBox,
arm64 musl source, the runc source, and the Go arm64 bootstrap archive.

The reproducibility manifest records the patch series, configuration inputs,
and generated artifact hashes. Build transcripts are evidence, not inputs.
