# Prepare a language build

The standard language recipes add instrumentation so Harmony can interrupt
execution inside application code, including loops that make no system calls.
Choose one that matches your project:

| Language | Starter project | What the recipe supplies |
| --- | --- | --- |
| C | `main.c` and local headers | Clang coverage callbacks and SDK forwarding |
| Rust | One Cargo package with `src/main.rs` | A pinned compiler, SDK dependency, and coverage passes |
| Go | One main package with `go.mod` | An instrumented compiler wrapper and SDK |
| Python | `main.py` and Python sources | Instrumented CPython and a source coverage catalog |
| Java | A prebuilt `app.jar` | Instrumented HotSpot with loop callbacks |

The [application guide](application.md) uses the C recipe. For toolchain details
and tested limitations, see the
[language recipes README](https://github.com/pH14/harmony/blob/main/workloads/languages/README.md).
The language build and shared runtime occupy separate layers, so updating the
runtime needn’t rebuild every language toolchain.

## Adapt the build to your project

For a more involved build, such as a Rust workspace or a Python application with
native extensions, set `workload.options.build.dockerfile` to your custom
Dockerfile. C, Rust, and Go also accept a `build.command` argument array that
produces `/out/application` inside the builder. Java projects needing additional
module setup can use a custom Dockerfile too.

Keep the symbols and instrumentation metadata when assembling the final image.
Every application binary needs its own instrumentation; putting an ordinary
production binary in a prepared image won’t instrument it. Admission checks the
executables and their attestations, but it cannot establish that every code
path has useful instrumentation.

You’ll need Docker or Podman for preparation. If you use `--offline`, Harmony
won’t download release assets, but the container builder can still use the
network.
