<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Instrumented language images

These fixtures exercise application loops through the Antithesis libvoidstar ABI. One thread spins without application system calls while another sleeps for 10 ms and prints twenty ordered markers. Coverage callbacks advance guest virtual time, so the timer thread can wake. The C fixture's `processes` argument exercises the same behavior across a fork.
The C check also runs the GCC-built fixture linked directly to libvoidstar's
trace-pc callback.

Build a fixture with `bash workloads/languages/build-image.sh c` or `rust`, then run `bash workloads/languages/run-check.sh harmony-language-c:local evidence/c`. Set `HARMONY_GUEST_DIR` to a qualified guest artifact directory and `HARMONY_BINARY` to the CLI binary if necessary. Checks require image admission, identical serial logs and full execution records from two fixed-seed boots, and another instrumented thread progressing while one is held at an event site. The parked-thread launcher checks cumulative callbacks because AFL coverage buckets eventually saturate.

Each Dockerfile produces a `language-base` image with the language binary and symbols. `image-key.py` hashes only that layer's build inputs. `compose.Dockerfile` copies the freshly built runtime and launcher last; `build-runtime.sh` builds the generic ABI shim and fault runtime once, shared with the etcd and SQLite historical recipes. Language code loads `/usr/lib/libvoidstar.so` at runtime.

The C fixture includes the unmodified, pinned upstream forwarding header. Rust uses the pinned upstream instrumentation crate and documented LLVM flags, with an explicit target triple to keep host build scripts uninstrumented. Recipes preserve unstripped binaries and their producer's `*.sym.tsv` files under `/symbols` and attest their installed paths with SHA-256. Debian's unused `libmemusage.so` diagnostic library is removed because it contains raw counters.

All target images need hidden instructions confined to reviewed digest/site pairs, no runtime code generation, and callbacks in every application loop. Admission scans every ELF, including libraries. Static checks detect forbidden instructions and writable executable ELF segments and stacks; runtime settings and memory-map checks establish the no-code-generation property of each supported recipe. Precompiled libc and Rust standard-library loops remain uninstrumented and bounded by their input.

`Checks / Harmony Workloads / Languages` builds or restores each layer in a 45-minute artifact prerequisite, composes the current runtime, and hands images plus an exact-source guest runtime to 15-minute language checks. Weekly and `rebuild_images` runs force cold language builds. The preparation skill lives in `.agents/skills/preparing-workloads`; its references follow validated recipes.
