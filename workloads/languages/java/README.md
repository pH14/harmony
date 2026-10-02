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
  `jlink` and `javac` steps and the rewriter.
- All native code, including `libjvm.so`, is compiled with Clang
  `-fsanitize-coverage=trace-pc-guard`. GC, compiler and other VM threads reach
  libvoidstar through these callbacks.
- The Antithesis C forwarding header is compiled once without coverage and
  linked from a static archive. The JDK passes extra linker flags twice for
  `libjvm.so`, and an archive member is linked only once.
- The JDK is built without DWARF. Harmony reads only the `nm` symbol tables
  under `/symbols/native`, and DWARF for `libjvm.so` adds about 270 MB to the
  guest root filesystem.
- The image holds a `jlink` runtime with only `java.base`.

## Back-edge callbacks

`harmony-coverage.patch` adds `java.lang.HarmonyCoverage` and two counters on
`java.lang.Thread`. `HarmonyCoverage.tick()` counts down on the current carrier
thread. When the count reaches zero, it calls `harmony_coverage_add` in
libvoidstar through libjava and stores the returned batch size. C2 inlines the
countdown, so a back-edge costs a few instructions.

`rewriter/CoverageRewriter.java` uses ASM to insert a call to `tick()` before
every jump or switch with a backward target. It rewrites every class in
`java.base`, through `jmod extract` and `jmod create` before `jlink`, and the
application classes. Every infinite Java execution passes a backward jump, so
the interpreter, C1 and C2 all yield. Run the rewriter on a workload's class
directories as well, extracting jars first.

Bytecode generated at runtime, for example by proxy or serialization
libraries, is not rewritten. Loops in that code do not yield.

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
timeout, because the C2-compiled spin loop never exits to the VM. With
`-Xcomp`, two boots with the same seed run 11.5 million steps each and produce
identical serial logs and run records.

## Known limits

- Intrinsic stubs such as `arraycopy` have no callbacks. Their loops end after
  a bounded number of iterations.
- Only `java.base` is in the image. Add modules to the `jlink` command when a
  workload needs them, rewrite their classes, then rerun admission.
