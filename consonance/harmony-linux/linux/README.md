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

The shipped x86 and arm64 guest kernels run only inside Harmony. Their clocks,
execution timing, entropy timing, and counter confinement require Harmony host
interfaces at every boot; running either image as ordinary Linux on another
hypervisor or bare metal is unsupported. Missing or incompatible required
clock interfaces stop boot. Build-time traps-off images are explicit
instruction-test controls, and the driver KUnit images exclude the clock at
build time to isolate driver serialization on QEMU; neither is a deployable
runtime profile.

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
The platform BusyBox links the same musl on both architectures, so a host libc
update cannot change the initramfs bytes.

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
The Linux VMM rejects non-long-mode CPU records at snapshot publication and
before snapshot restore. State hashes can observe transient CPU modes during
boot without admitting them as restorable snapshots. These checks observe boundaries;
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

The experimental `NESTED_HOST_PROFILE=1` kernel merges
`x86-nested-host-config-fragment`, builds `KVM`, `KVM_INTEL`, and `KVM_AMD` into the kernel,
and publishes `bzImage-nested-host` separately after the instruction audit.
Built-in initialization preserves the no-modules boundary. It is mutually
exclusive with the traps-off and task-park profiles and requires a matching
nested-host VMM contract. The instruction baseline must qualify the newly
compiled KVM paths before publication; a failed audit is a qualification
failure, not permission to extend the allowlist without reviewing those paths.
The guest KVM reads its host TSC through `RDMSR(IA32_TSC)`, which the outer
Consonance MSR filter completes from virtual time. Its ordered counter accessor
keeps explicit memory fences. The optional VMX hardware preemption timer is
disabled by default, and the fixture verifies that it remains disabled;
hardware countdowns cannot use Harmony virtual counter deadlines. Nested-host
counter baselines are separate from the ordinary kernel's baselines, with
toolchain-specific selections through `HARMONY_RDTSC_ALLOWLIST` and
`HARMONY_RDRAND_ALLOWLIST`.
`HARMONY_BUILD_JOBS` bounds compiler parallelism for small shared-host proofs.
For a Harmony AMD SVM host, the kernel consumes the contract's frozen CPUID
TSC/crystal ratio and processor frequency before PIT or APIC calibration.
This path requires built-in AMD KVM, the Harmony clock request, AMD identity
and SVM; configurations without AMD KVM compile it out. The fixed crystal
frequency also initializes the local APIC period before the Harmony clock
registers, avoiding a native-counter calibration loop during early boot.
The same named SVM path accepts the empty type-1 PCI bus after its address
latch round-trip and skips physical AMD northbridge configuration and the
FCH reset-status probe. The virtual platform has no northbridge register,
FCH reset-reason register or type-2 configuration ports.
Guest KVM invalidates the memory slot's cached shadow mappings after every
automatic bitmap drain while the Harmony clock is active. Hosted Intel can
otherwise omit the five pages written by a cold inner restore's readback step,
leaving the next incremental restore with an empty page plan. Rebuilding the
mappings rearms dirty tracking without replacing the inner VM or copying all
RAM. This adds MMU faults inside the special guest kernel; manual dirty-log
protection and stock-clock KVM behavior retain their existing semantics.
`build-nested-host-fixture.sh OUTPUT` packages `nested-kvm-check.c` with a static
`NESTED_HOST_BUSYBOX`; the check opens `/dev/kvm`, requires nested state and
KVM-supported Intel VMX or AMD SVM with NPT, and creates one VM. The fixture
checks the disabled hardware preemption timer on Intel and reports it as
inapplicable on AMD. AMD SEV is disabled; this profile hosts ordinary nested
VMs without memory encryption. The Nested Host Qualification workflow builds and boots
these exact inputs on an x86 runner.
The fixture also includes `nested-kvm-cache-check.c`. With
`harmony_nested_cache_check` on the kernel command line, it runs a minimal L2
through twelve port exits and prints each completed step without reading L2
segment state. L2 increments DS before each exit and reports it as the step
number, so L2 fails the check if it resumes with a segment value it did not
save. The outer cache regression captures after step three, detours through
step five, checks that direct restore preserves every nested-state byte, and
runs the restored fixture to step twelve. This helper isolates host VMCS synchronization; the OCI milestone checks
continue to use the production inner Consonance driver.

`trace-nested-vmcs.sh start` adds host kprobes on Intel nested VM entry.
Each `vmcs12_enter` event records the first 1 KiB of the vCPU's cached VMCS12
and the host's VMCS12 dirty, rare-field sync, and VMCS02 initialization flags.
Each `vmcs02_run` event records the loaded VMCS page and the host's queued
interrupt, exception, and NMI state before the host enters L2. A host that uses
Hyper-V enlightened VMCS keeps that page in its documented memory layout, so
the event holds the guest state the host asked the hypervisor to enter.
`vmcs02_exit` records the same state when an L2 entry fails, and the
`inject_*` events record each event the host injects into L2. The script finds
offsets from the `kvm_intel` BTF with `pahole`. A failed nested entry reflected
to L1 is not dumped by `dump_invalid_vmcs`, so these events are the only record
of the state on both sides of that entry. The `kvm_nested_vmexit_inject`
tracepoint records each exit reason between entries.
`trace-nested-vmcs.sh stop` removes the probes.

These targets remain direct substrate checks and are independent of the OCI
runtime assembly:

The OCI runtime assembler accepts `HARMONY_RUNTIME_KERNEL` to identify an
explicit kernel profile in its manifest, including `bzImage-nested-host`.
The platform init and supervisor remain explicit, workload-free inputs.

```sh
make -C consonance/harmony-linux/linux image
make -C consonance/harmony-linux/linux test
make -C consonance/harmony-linux/linux arm64-image
make -C consonance/harmony-linux/linux exec-image
make -C consonance/harmony-linux/linux go-runtime-image
```

`make test` checks reproducible x86 artifacts and verifies that boot on QEMU
without Harmony's clock interface stops before `/init`. The `vmm-core` guest
boot tests cover successful registration inside Harmony.

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

The nested-host GitHub qualification job uses Ubuntu 22.04 with an explicit
KVM-supported VMX/SVM and nested-state capacity check before building. Its pinned
Debian compiler container builds the matching GCC 14 kernel and fixture using
the nested profile's reviewed instruction baselines. The ordinary guest kernel
keeps its existing toolchain profiles. Compilation and instruction audits finish
before the job attempts to boot L1 or run the inner VMM.
