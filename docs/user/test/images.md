# Use an existing image

An OCI image supplies the application's filesystem and executables. It can be
a registry image, a saved Docker archive, or an OCI layout. The faults workload
uses the recipe's node and lifecycle commands, or the image's supervisor bundle
when those commands are omitted.

## Check the image's contract

The executable must match the guest architecture. Instrumentation must reach
application loops, and the image must retain the required runtime, symbols,
and attestations. Hidden entropy instructions must be absent or covered by the
reviewed instruction rules. Generated machine code needs a supported,
reviewed runtime.

A container that works under ordinary Docker is not automatically a supported
Harmony workload. Use the [language preparation path](languages.md) where it
fits, or follow the existing image recipes when building a custom image.

The [supervisor contract](https://github.com/pH14/harmony/blob/main/consonance/harmony-linux/supervisor/README.md)
defines bundle syntax and lifecycle behavior. Most users can express those
commands in [the recipe](../reference/recipe.md) and let the CLI build the bundle.

## Preserve investigation tools

Guest commands use the tools inside the image. Include a shell and the
application-specific tools you need for interactive diagnosis. A runtime log-level
control lets you increase logging on a branch without rebuilding the application.
Its effect is still a change to that branch's execution.

Treat image tags as preparation inputs. Saved experiments retain resolved
artifacts and identities; investigation must not silently fetch a newer image
under the same tag.
