---
name: harmony-build
description: Produce a pinned, reproducible workload image Harmony can execute, and inventory what is actually instrumented. Use when packaging an application for Harmony, choosing compiler or runtime instrumentation, or checking that a build's artifacts match what runs. Covers the image format, pinning inputs, symbols and runtimes, and Harmony's real instrumentation support.
---

# Building a workload image

Harmony boots one OCI image inside a deterministic VM. Everything the workload
needs at run time must be inside that image; nothing is fetched during a run.

## The shape of the deliverable

An OCI image — a registry reference, an OCI layout directory, or a Docker image
archive — carrying:

- every executable and library the workload runs, including its interpreter or
  language runtime,
- its data directory or fixture, already initialized if initialization is slow,
- `/etc/harmony/bundle`, described in [`harmony-instrument`](../harmony-instrument/SKILL.md),
- any checker command the bundle's hooks name.

The image's init mounts `/proc`, `/sys` and `/dev`, binds the image's rootfs,
and chroots into it. A dynamically linked binary therefore needs its loader and
libraries in the image; nothing on the build host is visible in the guest.

Stage and check the image without a VM:

```bash
harmony preflight
```

`preflight` names the host's support-matrix cell and whether the guest kernel
and base initramfs are installed. Execution needs a Linux x86-64 or aarch64
KVM host; preparation, bundle checking, and workspace reading run anywhere.

## Pin every input

Record, and keep with the image:

- the exact source archive and its hash,
- compiler and toolchain versions,
- every build flag,
- the digests of base images and of each dependency package,
- the digest of the produced image.

A campaign's `report.json` pins the prepared image, kernel, and agent hashes
and the execution identity. Those identities are what makes a recorded finding
replayable; a rebuilt image that hashes differently is a different workload,
and Harmony will say so rather than substituting it.

Build with the toolchain's own reproducibility switches where they exist —
fixed timestamps, sorted archive members, no build paths embedded in output.

## Inventory before you instrument

List every executable and shared library in the image with its language,
compiler, runtime, and build system. For each, record one of:

- **instrumented** — built with instrumentation Harmony has a supported path
  for, with the runtime it needs present in the image,
- **cataloged only** — built and shipped without instrumentation, with the
  reason,
- **deliberately uninstrumented** — scripts, data, or components where
  instrumentation cannot reach a Harmony consumer.

Say which, and why. An inventory that marks everything instrumented without
showing observations arriving is worse than one that admits a gap.

## What Harmony actually supports today

**Semantic checks.** Supported and demonstrated: hook stdout directives through
the fault agent. This is the route to use.

**The SDK ABI from inside the guest.** `libvoidstar.so` implements the public
SDK ABI — `fuzz_json_data`, `fuzz_get_random`, and the `trace-pc-guard`
sanitizer callbacks — over `/dev/harmony`, which is visible inside the workload
chroot. The fault package does **not** install that library into a workload
rootfs. To use it, your image must carry it, and you must show reports arriving.

**Compiler coverage.** The callback ABI exists. The faults package's search
does not consume basic-block identities, so instrumenting a binary with
`-fsanitize-coverage=trace-pc-guard` produces callbacks that reach no consumer.
Compiling with the flag is not coverage-guided exploration. Do not claim it.

**Scheduling control** is a separate capability from coverage. The `Park`
action holds a guest thread at an execution place named by address; that is the
supported mechanism, and it comes from the fault action alphabet, not from
compiler instrumentation.

If you instrument anyway, keep DWARF symbols and a build id so a report can be
mapped back to source, and check that stripping did not remove them.

## Prove the built artifact is the one that runs

The common failure is instrumenting one build and shipping another: a distro
package shadows the local build, a wrapper script runs a different path, a
container layer overwrites the binary. Before a campaign:

1. Resolve the executable the bundle's `node` line actually runs, following
   symlinks and wrappers.
2. Check that file's identity — hash, build id, presence of the expected
   symbols — inside the staged image, not in the build directory.
3. Run one short campaign and confirm the observations you expect appear.

## What you leave behind

A pinned recipe that rebuilds the image, the image itself, the per-component
inventory with reasons, and evidence that the executed artifact is the one you
built.
