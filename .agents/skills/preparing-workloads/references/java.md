# Java

The recipe is `workloads/languages/java/Dockerfile`. It builds OpenJDK 21 from the pinned `jdk21u` tag with the HotSpot server VM, including the C1 and C2 JIT compilers. `workloads/languages/java/README.md` has the measurements.

## Build

Configure with `--with-jvm-variants=server --with-toolchain-type=clang --with-native-debug-symbols=none`. Pass `-fsanitize-coverage=trace-pc-guard` through `--with-extra-cflags` and `--with-extra-cxxflags`. This instruments the VM's own threads: GC, compiler and runtime.

Link the forwarding header from a static archive compiled without coverage. Pass it through `--with-extra-ldflags` together with `-Wl,--undefined=__sanitizer_cov_trace_pc_guard_init`. The JDK adds extra linker flags twice for `libjvm.so`, so an object file in that list causes duplicate symbols.

Use a pinned stock JDK 21 for `--with-boot-jdk` and `--with-build-jdk`, and for the `jmod`, `jlink` and `javac` steps.

## Back-edge callbacks

JIT-compiled code has no Clang callbacks. Apply `harmony-coverage.patch`. It adds `java.lang.HarmonyCoverage.tick()` and `jdk.internal.misc.HarmonyRewriter`, which inserts `tick()` before every backward jump. The JDK rewrites every class it defines from bytes, including application classes, lambdas and method-handle loops. Boot classes skip that path: run `HarmonyRewriter` over each extracted jmod in the image before `jlink`, as the recipe does for `java.base`.

## Machine code at runtime

Run `check-code-generator.sh` on the JDK source. It fails when HotSpot's x86 or aarch64 assembler can encode an entropy instruction. Rerun it, and review any failure, when the JDK tag changes. The fixture checks `/proc/self/maps`: no writable executable file mapping, and anonymous executable mappings within the code cache size.

## Known limits

- Intrinsic stubs such as `arraycopy` have no callbacks. Their loops are bounded.
- Only `java.base` is in the image.

## Acceptance

```sh
bash workloads/languages/build-image.sh java
bash workloads/languages/run-check.sh harmony-language-java:local evidence/java
```

`run-check.sh` also boots the fixture twice under `-Xcomp`, and once with its spin loop in a `MethodHandles.whileLoop`.
