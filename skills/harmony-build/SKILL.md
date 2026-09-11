---
name: harmony-build
description: Prepare reproducible Harmony CLI, workload, and guest artifacts from the package and host contracts.
---

# Harmony builds

Use this skill when preparing an artifact set that another Harmony run can
repeat. Read the selected package’s README, manifest, input format, and
backend adapter first. Record the source revision, build system, toolchain,
compiler flags, dependency versions or digests, generated inputs, and image
recipe. Pin every input that can affect the executed bytes.

Package the complete guest closure: the executable named by the workload,
its loader and shared libraries or language runtime, required data, the
workload bundle, and any checker commands it invokes. Nothing should be
fetched from the host during a guest run. Build the staged image using the
package’s instructions and keep its final digest with the recipe.

Use a usable installed `harmony` when its identity and host support satisfy the
task; build it from a checkout with `cargo build --release -p harmony-cli` when
it is absent or the source revision is part of the requirement. Run
`harmony preflight` to inspect host readiness and installed guest artifacts,
understanding that preflight does not execute the workload. Supply controlled
kernel, base-image, and package-agent paths through the documented flags or
`HARMONY_GUEST_DIR` as applicable.

Before running, resolve the path the staged bundle actually executes,
following wrappers and symlinks. Compare that file’s digest, build ID, or
other available identity with the artifact produced by the recipe, inside the
staged image. This catches a package shadowing the newly built binary or a
checker/runtime missing from the image. Keep a fresh output directory and
retain the stream, report, and package-owned identity artifacts.

Treat compiler or coverage instrumentation as a separate, unqualified claim
unless the packaged runtime emits data that the selected Harmony search
consumes and the run demonstrates that path. Compiler success, symbols,
callbacks, and flags alone do not establish search feedback or deterministic
coverage. Hand off the exact recipe, artifact paths, identities, backend,
input, host requirements, and output directory.
