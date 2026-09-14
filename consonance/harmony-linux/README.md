<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# harmony-linux

`harmony-linux` contains the guest-side environment for consonance: pinned
Linux sources and image builders, the `/dev/harmony` integration, guest agents,
and the no-std SDK used by those agents. Bare-metal acceptance payloads and
instruction audit tables live alongside the scripts in this component.

## Entry points

```sh
make -C consonance/harmony-linux fetch
make -C consonance/harmony-linux test-libvoidstar
make -C consonance/harmony-linux test-linux
make -C consonance/harmony-linux test
```

`fetch` downloads and verifies the pinned platform kernel, generic userland,
and static OCI runtime sources. Workload packages own their separate fetch
entrypoints.
`test-libvoidstar` runs the portable ABI and device-transaction checks.
`test-linux` builds the Linux artifacts twice and runs the image gate; it
requires Linux, or a Linux/amd64 build container on macOS. Build output lives
in `consonance/harmony-linux/build/`; `GUEST_BUILD_ROOT` can select another
build root.

The root `flake.nix` provides separate locked platform and workload image
entrypoints on native Linux:

```sh
nix run .#platform-guest-images -- --output "$PWD/platform-output"
nix run .#workload-guest-images -- --output "$PWD/workload-output"
```

On Linux/x86_64 the platform builder produces the standard platform kernel and
direct fixture initramfs, plus the canonical OCI runtime artifacts when their
runtime inputs are supplied. Native Linux/aarch64 produces the corresponding
`aarch64` directory. The workload builder owns application image recipes and
application source inputs; those inputs are outside the platform closure. The
existing `.#guest-images` app remains an alias for the platform builder while
callers migrate to the explicit names.

The pinned BusyBox source is also available as a standalone flake package for
reproducible image preparation and CI reuse:

```sh
nix build .#busybox-source --no-link --print-out-paths
```

The resulting store path is the hash-verified `busybox-1.38.0.tar.bz2` source
from the same pin used by the guest image builder. Workload acceptance fetches
that archive from Buildroot's mirror, with the Nix package as a fallback, and
checks the same lock-file SHA-256 before building. An upstream download outage
therefore does not change the accepted source bytes.

## Components

- `linux/` builds the pinned kernels, direct platform fixtures, and the
  workload-free OCI runtime. Kernel patches provide the guest device,
  paravirtual clock, observation, and task-park interfaces; build scripts
  verify source and artifact hashes.
- `libvoidstar/` implements the SDK-facing dynamic ABI and communicates with
  `/dev/harmony`.
- `sdk/` provides the no-std event, state, assertion, lifecycle, and entropy
  hooks used by guest payloads.
- Workload packages under `workloads/` build their own images and fetch their
  own application inputs after the platform artifacts are prepared.

The guest transport is synchronous and serialized by the kernel driver. Guest
entropy comes from the host-provided seeded service; the compatibility library
does not provide a host-randomness fallback.

Workload image recipes that use GNU cpio require version 2.14 or newer so
`--reproducible` normalizes inode, device, and directory-link metadata before
an image hash is recorded.

Cold Nix guest builds fetch the pinned BusyBox archive from the Buildroot mirror
with the upstream URL as fallback. Both locations use the same locked SHA-256;
the mirror choice leaves the guest source version and bytes unchanged.

## XSAVE consistency qualification in progress

The September 14 workstream keeps AVX and targets deterministic guest-owned
XSAVE buffers on hosted AMD as well as Intel. The broad snapshot integration
goal remains open; this work does not qualify arbitrary supplied workloads.

Qualification proceeds through a traced fixed-CPU reuse cohort (D1), a
bare-metal AMD attribution run (D2), synthetic whole-buffer canonicalization
with init/SSE-active/AVX-active states (D3), and an inventory of kernel save
sites, XGETBV(1), signal-frame handling, and userspace loader behavior (D4).
The fixed-CPU trace in run 34839425407 confirmed nested-page-fault exits at
the XSAVE instruction on hosted AMD EPYC 9V74 in reference and cold runs,
with none in the reused run. Prefaulting must now establish equivalent
fixtures without guest warmup. No guest canonicalizer is shipped before D3
and D4.

The proposed guest contract compares identity at guest-initiated exits; debug
stops are interventions, not comparison points. Patched save/canonicalize
sequences must exclude guest interrupt handlers. Admitted executable code,
including shared libraries and dlopen dependencies, must exclude XSAVE-family
instructions and XGETBV(1) outside the patched kernel. XGETBV(0) needs a proven
zero selector. JITs, generated code, and writable executable mappings are
outside that controlled admission scope. These are requirements to implement
and verify, not claims about the current image or scan.

The primary path adapts whole-buffer canonicalization to each audited save
layout and fault path, disables modified-save optimizations, and uses eager
dynamic binding in admitted workloads. Disabling XSAVE/AVX is a fallback only
if a correct synthetic canonicalizer exposes actual register-value corruption.
Raw restore presence stays in the restore path and diagnostics. Removing it
from logical identity requires the kernel audit and admission argument, not
merely matching boot tests. Real-Linux paired boots and interrupted runs must
then agree in complete RAM and modeled state on every qualification host.

The pinned Linux 6.18.35 source audit identifies the general kernel fpstate
save, direct user signal-frame save, and independent XSAVES for Intel LBR.
The XGETBV(1) consumers are dynamic signal-mask pruning and AMX idle release.
Plain XSAVE writes every requested component; optimized forms have different
write rules. Signal save already uses plain XSAVE, but its mask and subsequent
ABI header rewriting need separate treatment. Disabling local interrupts alone
does not settle the LBR/NMI path or fault-safe user-memory writes. Final guest
configuration and executable disassembly must bind the audit to shipped bytes.

Static linking alone does not establish the absence of internal XSAVE or
IFUNC save paths. The current image pipeline does not yet enforce the proposed full dependency
instruction admission, eager-binding, or generated-code policy. Source review
and synthetic success cannot substitute for those artifact-level checks.
