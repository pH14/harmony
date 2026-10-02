<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Java language recipe

The recipe builds OpenJDK 21 from the pinned `jdk21u` source tag with the Zero
VM. Zero is the C++ bytecode interpreter in HotSpot. It has no JIT compiler and
no template interpreter, so the JVM writes no machine code at runtime.

```sh
bash workloads/languages/build-image.sh java
bash workloads/languages/run-check.sh harmony-language-java:local evidence/java
```

## Build

- A pinned Temurin 21 image is the boot JDK. It also runs the build's `jmod`
  and `jlink` steps and compiles the fixture. These tools run very slowly on
  the instrumented Zero JDK.
- All native code, including `libjvm.so`, is compiled with Clang
  `-fsanitize-coverage=trace-pc-guard`. Every Java loop runs through the
  instrumented interpreter loop, so it reaches libvoidstar.
- The Antithesis C forwarding header is compiled once without coverage and
  linked from a static archive. The JDK passes extra linker flags twice for
  `libjvm.so`, and an archive member is linked only once.
- The image holds a `jlink` runtime with only `java.base`.
- Each native file keeps an unstripped copy and an `nm` symbol table under
  `/symbols/native`.

## No runtime code generation

Zero still reserves a 160 KiB code cache, and HotSpot maps it writable and
executable. Zero stores only data there. `zero-code-cache.patch` maps that
reservation without execute permission on Zero builds.

The fixture checks that it runs on Zero and reads `/proc/self/maps` before and
after its run. It fails on any writable executable mapping and on any anonymous
executable mapping other than vDSO and vsyscall.

## Known limits

- Zero runs a simple loop at about 24 ns per iteration on an arm64 host with
  libvoidstar absent. HotSpot's interpreter takes about 12 ns and its JIT about
  1 ns.
- Inside the guest, where every callback reaches libvoidstar, the same loop
  takes about 340 ns of wall time per iteration. Each iteration advances the
  guest clock by about 6 µs.
- Zero does not support continuations, so each virtual thread runs on its own
  OS thread. 1000 waiting virtual threads use 1000 OS threads.
- Only `java.base` is in the image. Add modules to the `jlink` command when a
  workload needs them, then rerun admission.

Measure the loop and the virtual-thread count with a program that times a loop
with `System.nanoTime` and counts `/proc/self/task` while virtual threads wait on
a latch.
