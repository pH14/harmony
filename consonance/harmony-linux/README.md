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
`test-linux` builds the Linux artifacts twice and runs the image check; it
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

The x86 Nix OCI producer packs the minimal Linux fixture with the same reviewed
BusyBox binary as the OCI platform. Its fixed init only mounts proc/sysfs,
prints readiness and powers off; the archive contains no libvoidstar or dynamic
dependencies. `build-initramfs.sh --busybox FILE` selects that binary explicitly;
the standalone builder still builds its own BusyBox.

## x86 XSAVE behavior

The guest kernel canonicalizes complete XSAVE buffers while keeping AVX. The
x86 configuration uses `noxsaveopt noxsaves`; save/canonicalize sequences exclude
local interrupt handlers. Identity comparisons belong at completed
guest-initiated exits, not at diagnostic breakpoint stops inside those sequences.
The kernel configuration excludes unsupported LBR/AMX save paths and kexec.
The CLI, client defaults and workload adapters supply
`noxsaveopt noxsaves LD_BIND_NOW=1`; the OCI runtime also preserves eager binding
through supervisor startup, workload startup and runc re-execution.

The general fpstate and user signal-frame paths have distinct layouts and fault
handling. Successful paths overwrite raw bitmap register aliases and owned
stack storage before restoring interrupts. A conditional signal checked-access
failure can retain the raw bitmap in EDX. The x86 execution contract
excludes that post-success failure: successful mask-7 plain XSAVE has already
written both possible buffer pages, with one vCPU, disabled local IRQs, fixed
mappings, no PKU and healthy memory. Initial XSAVE faults precede the bitmap
read and do not establish coverage of this later conditional failure. External
NMI/MCE injection and arbitrary imported pending events are outside this
contract; the generic backend can represent them.

Outside the patched kernel, workloads must not execute XSAVE-family saves or
XGETBV with ECX=1. XGETBV with a proven zero selector remains allowed. Static
linking alone does not exclude libc save routines; glibc's resolver trampolines
hold XSAVE instructions but never run, because every image boots with
`LD_BIND_NOW=1` and the scanner rejects TLSdesc relocations. Generated code,
JITs, code mutation and writable executable memory are excluded by policy. These
restrictions do not qualify arbitrary images, SQL, ROMs or imported machine
states.

Raw XSAVE presence remains in restore data. Snapshot identity uses the core
layer's [logical identity](../vmm-core/README.md#published-xsave-identity-check),
which excludes only validated init x87/SSE presence metadata.
Matching finite executions does not establish general continuation equivalence.
Outstanding XSAVE and PAE behavior is tracked in
[#307](https://github.com/pH14/harmony/issues/307) and
[#314](https://github.com/pH14/harmony/issues/314).

## x86 userspace admission

Run the scanner on complete rootfs trees, including shared libraries and possible
`dlopen` inputs, with GNU objdump available:

```sh
python3 consonance/harmony-linux/scripts/x86-xstate-admission.py inventory ROOTFS --output candidate.json
python3 consonance/harmony-linux/scripts/x86-xstate-admission.py verify ROOTFS --output result.json
python3 consonance/harmony-linux/scripts/x86-xstate-admission.py inventory-initramfs INITRAMFS --output candidate.json
python3 consonance/harmony-linux/scripts/x86-xstate-admission.py verify-initramfs INITRAMFS --output result.json
```

`--objdump` selects the disassembler. Inventory discovers every ELF by file
contents regardless of executable permissions and records file digests, metadata,
symlink targets and every XSAVE-family instruction site. Verify checks the
executable properties listed below on that same inventory. It compares nothing
against a stored digest, so rebuilding an image does not require a new approval.
Actual launch configuration, kernel, ordered archive composition, ROM and SQL
inputs are additionally checked by the workload composition checker under
`workloads/guest-images/`.

Dependency analysis resolves symlinks inside the guest root and checks the
complete transitive ELF closure. The supported dynamic interpreter is
`/lib64/ld-linux-x86-64.so.2`, with default directories
`/lib/x86_64-linux-gnu`, `/usr/lib/x86_64-linux-gnu`, `/lib` and `/usr/lib`.
Missing dependencies, differing duplicate SONAMEs and ambiguous resolutions
fail closed. Other interpreters, loader caches, RPATH/RUNPATH and glibc-hwcaps
search are unsupported. Launch environment overrides are outside the contract.

Executable segments are scanned for XSAVE-family instructions and XGETBV;
restore instructions are inventoried separately. Verification rejects W+X
PT_LOAD segments, executable PT_GNU_STACK, DT_TEXTREL/DF_TEXTREL images and
TLSdesc relocations anywhere in the closure. Every XGETBV site must have a
recognized straight-line ECX-zero sequence. The bounded recognizer rejects
intervening ECX writes, branches, calls, unknown instructions and observed
alternate direct entries. Trusted control flow must also exclude indirect entry
that bypasses initialization; linear disassembly cannot enforce that condition
for arbitrary code. XSAVE-family sites are inventoried without rejection: the
glibc resolver trampolines that hold them never run, because every image boots
with `LD_BIND_NOW=1` and carries no TLSdesc relocation.

Save instructions may occur only inside exact digest-bound resolver regions,
with `kind`, `start`, `size` and `sha256`. `eager-resolver` requires startup eager
binding and the fixed loading paths to keep the region unreachable.
`unused-tlsdesc` requires no TLSdesc relocations anywhere in the complete ELF
closure; the scanner rejects that exception if such relocations are present.
Eager binding alone does not establish TLS descriptor unreachability. Neither
exception is a symbol-name allowlist or runtime instruction interception.

The initramfs adapter accepts one raw newc `070701` archive or its gzip encoding
and binds the exact input bytes. It inventories device nodes as metadata without
creating or opening host devices. ELF analysis uses a temporary projection of
regular files, directories and symlinks; guest metadata comes from the archive.
Traversal, duplicate entries, hardlinks, truncation, nonzero padding,
concatenated archives and unsupported types fail closed. Absolute symlinks retain
guest-root semantics; one terminal NUL in a kernel-generated symlink body is
accepted, while embedded NULs are rejected. Newc has no extended attributes.

These static checks do not enforce runtime filesystem immutability, W^X,
startup binding or the no-generated-code policy. The OCI root is writable;
execution remains limited to trusted, fixed workloads and loading behavior.

```sh
python3 consonance/harmony-linux/scripts/test_x86_xstate_admission.py -v
```

The scanner tests require GNU objdump; `OBJDUMP` selects a non-default executable.
