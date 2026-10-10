# Use an existing image

You can supply an application as a registry image, a saved Docker archive, or an
OCI layout. Harmony uses its filesystem and executables, along with the node
and lifecycle commands in your recipe. If you omit those commands, the faults
workload uses the image’s supervisor bundle.

## Prepare the image for Harmony

An image needs more preparation than it would for an ordinary Docker run. Its
executables must match the guest architecture and have instrumentation that
reaches application loops. It must also retain the runtime, symbols, and
attestations required for admission.

Instructions that introduce hidden entropy must be removed or covered by the
reviewed instruction rules. Runtimes that generate machine code need a supported,
reviewed implementation. The [standard language recipes](languages.md) handle
these requirements for supported builds; use the existing image recipes as a
starting point for a custom build.

You can usually describe startup and lifecycle commands in
[your recipe](../reference/recipe.md) and let the CLI create the supervisor
bundle. If you need to supply the bundle yourself, see the
[supervisor contract](https://github.com/pH14/harmony/blob/main/consonance/harmony-linux/supervisor/README.md)
for its format and behavior.

## Include the tools you’ll need to investigate

Guest commands run with the tools inside the image, so include a shell and any
utilities you’ll want when examining a failure. A runtime log-level control is
also useful: it lets you turn up logging on a branch without rebuilding the
program. The extra logging can affect that branch’s execution.

An image tag identifies what to prepare. Saved results retain the resolved
artifacts and their identities so that investigating an old finding won’t
silently pick up a newer image under the same tag.
