<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Java language recipe

The recipe builds OpenJDK 21 from the pinned `jdk21u` source tag with the
HotSpot server VM, including the C1 and C2 JIT compilers. Compiled Java code has
no Clang coverage callbacks, so a build-time rewriter adds a callback at every
bytecode back-edge.

```sh
bash workloads/languages/build-image.sh java
bash workloads/languages/run-check.sh harmony-language-java:local evidence/java
```

## Build

- A pinned Temurin 21 image is the boot JDK. It runs the build's `jmod`,
  `jlink` and `javac` steps.
- All native code, including `libjvm.so`, is compiled with Clang
  `-fsanitize-coverage=trace-pc-guard`. GC, compiler and other VM threads reach
  libvoidstar through these callbacks.
- The Antithesis C forwarding header is compiled once without coverage and
  linked from a static archive. The JDK passes extra linker flags twice for
  `libjvm.so`, and an archive member is linked only once.
- The JDK is built without DWARF. Harmony reads only the `nm` symbol tables
  under `/symbols/native`, and DWARF for `libjvm.so` adds about 270 MB to the
  guest root filesystem.
- The image defaults to a `jlink` runtime containing `java.base`. Set the
  `JAVA_MODULES` build argument to a comma-separated module list for a service.
  The linker resolves transitive dependencies and rewrites every included
  module, including its native libraries in the symbol manifest. OpenJDK
  builds all jmods once so changing the selected modules reuses that build.
- Rewriting changes module hashes. `ModuleHashes.java` removes the original
  hash attributes from extracted module descriptors; `jmod hash` recomputes
  the hashes over the rewritten modules before the final link.

## Back-edge callbacks

`harmony-coverage.patch` adds `java.lang.HarmonyCoverage`, two counters on
`java.lang.Thread`, and `jdk.internal.misc.HarmonyRewriter`.
`HarmonyCoverage.tick()` counts down on the current carrier thread. When the
count reaches zero, it calls `harmony_coverage_add` in libvoidstar through
libjava and stores the returned batch size. C2 inlines the countdown, so a
back-edge costs a few instructions.

`HarmonyRewriter` uses the JDK's internal copy of ASM to insert a call to
`tick()` before every jump or switch with a backward target. Every infinite Java
execution passes a backward jump, so the interpreter, C1 and C2 all yield. The
JDK rewrites each class when it is defined from bytes: through
`ClassLoader.defineClass`, through `Lookup.defineClass` and hidden classes,
which covers lambdas, method-handle loops and proxies, and through
`Unsafe.defineClass`. Boot classes load from the runtime image without that
path, so the build runs `HarmonyRewriter` over the extracted `java.base` jmod
before `jlink`. `link-runtime.sh` applies the same rewrite to all selected
modules and their dependencies. Application classes and jars need no preparation.

A class that the rewriter cannot parse, such as a class file newer than Java
21, is defined unchanged. A malformed class file is also defined unchanged, so
the VM reports the same error it would without rewriting. Classes defined
through JNI `DefineClass` are not rewritten.

## Machine code at runtime

HotSpot writes machine code into its code cache, which is mapped writable and
executable. Admission cannot scan it. `check-code-generator.sh` runs on the
source before the build and fails when the x86 or aarch64 assembler can encode
RDRAND, RDSEED, RNDR or RNDRRS. The only `0F C7` encoding is `cmpxchg8` with
`/1`, and the only `mrs` reads are FPSR, FPCR, DCZID, CTR and NZCV.

The fixture checks that it runs on the server VM and reads `/proc/self/maps`
before and after its run. It fails on any writable executable file mapping, and
when anonymous executable mappings span more than the 240 MiB default code
cache.

## Measurements

Measured on an arm64 laptop with `Measure`, a loop of 20 million iterations
plus 1000 virtual threads waiting on a latch. All loops are rewritten.

| | Zero (previous recipe) | Server VM |
| --- | --- | --- |
| Loop, native, ns per iteration | 24 | 2.2 |
| Loop, guest clock, ns per iteration | about 6000 | about 400 |
| OS threads for 1000 waiting virtual threads, native | 1000 | 28 |

Without rewriting, the fixture prints its ready line and then hangs until the
timeout, because the C2-compiled spin loop never exits to the VM. The check runs
the fixture with `-Xcomp -XX:-TieredCompilation`, limited to `Fixture` and
`java.lang.invoke`. C2 then compiles the spin loop before it runs. Tier 3 code
would call into `libjvm.so` from its profiling counters, which reaches
libvoidstar and hides a loop without callbacks. Two such boots with the same
seed produce identical serial logs and run records. `Fixture generated` spins in
a `MethodHandles.whileLoop`, whose loop exists only in bytecode the JDK
generates at runtime. It prints all twenty markers, and hangs on a JDK that
does not rewrite classes at definition time.

## Known limits

- Intrinsic stubs such as `arraycopy` have no callbacks. Their loops end after
  a bounded number of iterations.
- The default runtime includes only `java.base`. Use `jdeps` on a workload
  and its dependency jars to choose `JAVA_MODULES`, then rerun admission and
  the language fixtures. Reflection and optional providers may require modules
  that static dependency analysis cannot discover.

## Diagnostic callback counts

`profile-callbacks.py INPUT OUTPUT` produces a diagnostic copy of
`libvoidstar/voidstar.c`. Compile that copy through the faults runtime and put
it in a separate diagnostic image. With `HARMONY_JAVA_COVERAGE_PROFILE=1` set
on a JVM, it creates `/tmp/java-coverage.PID`, containing four native-endian
64-bit counters: Java native callback calls, Java back-edge hits delivered,
Java calls that exchanged a coverage threshold, and all coverage exchanges.
Read the counters while the JVM is alive. The diagnostic uses atomic counters
and adds overhead; it does not change coverage thresholds, countdowns or
scheduler selections. Never substitute its image for a recorded production
image during replay.

A ZooKeeper startup with C1, Serial GC, one processor and a 32–128 MiB heap
recorded 7,349,177 Java back-edges in 393 native calls, of which 266 exchanged
a coverage threshold. All threads together made 5,149 coverage exchanges.
The diagnostic reached readiness in 9.93 guest seconds, versus 9.94 without
it. Java back-edge exchanges were about 5.2% of all coverage exchanges in that
probe, so there was no evidence to justify weakening the existing countdown.
The batching already follows the remaining budget returned by libvoidstar.
