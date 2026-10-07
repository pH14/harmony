# Prepare a language build

Use the standard recipe when it matches your project's shape. It supplies the
instrumentation needed for execution to yield inside application code, not just
at system calls. The application build and the shared runtime are separate
layers, so a runtime update need not rebuild every language toolchain.

| Language | Starter project | Preparation |
| --- | --- | --- |
| C | `main.c` and local headers | Clang coverage callbacks and SDK forwarding |
| Rust | One Cargo package with `src/main.rs` | Pinned compiler, SDK dependency, coverage passes |
| Go | One main package with `go.mod` | Instrumented compiler wrapper and SDK |
| Python | `main.py` and Python sources | Instrumented CPython and a source coverage catalog |
| Java | A prebuilt `app.jar` | Instrumented HotSpot with loop callbacks |

The [application guide](application.md) shows an executed C recipe. For the
language toolchains and their tested limits, see the maintained
[language recipes](https://github.com/pH14/harmony/blob/main/workloads/languages/README.md).

## When the starter recipe is too small

Use `workload.options.build.dockerfile` for a custom image build, including Rust
workspaces, native Python extensions, or Java modules needing more setup.
C, Rust, and Go also accept an explicit `build.command` argument array that
produces `/out/application` inside the builder.

Preserve symbols and instrumentation metadata. Copying an ordinary production
binary into an otherwise prepared image does not add instrumentation to that
binary. Image admission checks executable files and attestations; it cannot
prove that every code path has useful instrumentation.

Preparation requires Docker or Podman. `--offline` prevents Harmony release
asset downloads; it does not disable network access inside a container builder.
