# Java

The recipe is `workloads/languages/java/Dockerfile`. It builds OpenJDK 21 from the pinned `jdk21u` tag with the Zero VM, the C++ bytecode interpreter in HotSpot. `workloads/languages/java/README.md` has the measurements.

## Build

Configure with `--with-jvm-variants=zero --with-toolchain-type=clang`. Pass `-fsanitize-coverage=trace-pc-guard` through `--with-extra-cflags` and `--with-extra-cxxflags`. Instrumenting `libjvm.so` gives every Java loop a callback through the interpreter loop.

Link the forwarding header from a static archive compiled without coverage. Pass it through `--with-extra-ldflags` together with `-Wl,--undefined=__sanitizer_cov_trace_pc_guard_init`. The JDK adds extra linker flags twice for `libjvm.so`, so an object file in that list causes duplicate symbols.

Use a pinned stock JDK 21 for `--with-boot-jdk` and `--with-build-jdk`. Run the final `jlink` with that JDK as well. The build's own `jmod` and `jlink` steps run very slowly on the instrumented Zero JDK.

Build the `jmod` for each module the workload needs and link only those modules into the image. Compile application classes with the stock JDK's `javac`. Class files are data for the instrumented interpreter.

## No runtime code generation

Zero writes no machine code. HotSpot still maps its code cache writable and executable. Apply `zero-code-cache.patch`, which maps it without execute permission on Zero builds. Check `/proc/self/maps` in the fixture before and after the run.

## Known limits

- Zero is several times slower than HotSpot's interpreter, and each callback adds more inside the guest.
- Zero has no continuations. Each virtual thread runs on its own OS thread.

## Acceptance

```sh
bash workloads/languages/build-image.sh java
bash workloads/languages/run-check.sh harmony-language-java:local evidence/java
```
