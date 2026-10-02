---
name: preparing-workloads
description: Prepare a service or language workload image for deterministic Harmony execution, including instrumentation, symbols, reviewed instructions, and image admission. Use for new workload recipes or rejected image preparation; ordinary host setup is outside this skill.
---

# Preparing workloads

List every service before changing its image:

| Service | Language and version | Build system | Runtime artifact path | Symbol files |
| --- | --- | --- | --- | --- |

Include helper processes and native libraries that can run long loops. Record each service's source revision, startup command, readiness check, and existing Harmony bundle. Keep the user's workload and deployment choices.

Every target image must meet three conditions:

1. Hardware entropy instructions appear only at reviewed sites.
2. Machine code comes only from scanned files or from a reviewed code generator.
3. Every application loop reaches a libvoidstar callback.

Admission scans every executable and shared object in the image. It rejects writable and executable segments, executable stacks, and unreviewed RDRAND, RDSEED, RNDR or RNDRRS sites. Counter reads need no review, because the guest kernel traps them. Admission cannot scan code that a runtime generates. Turn off each code generator in the recipe, or review the generator's source for entropy encodings and check that review at build time, as the Java recipe does for HotSpot. Check `/proc/self/maps` in the fixture.

Read the reference for the language:

- [Compiled languages](references/compiled.md): C, C++, other LLVM front ends, Rust, and GCC.
- [Go](references/go.md): cgo forwarding, standard-library selection, and the etcd case.
- [Python](references/python.md): the CPython interpreter, source-built extensions, and the PostgreSQL driver.
- [Java](references/java.md): the OpenJDK server VM with bytecode rewriting.

Install the composed runtime at `/usr/lib/libvoidstar.so` with `workloads/languages/compose.Dockerfile`. Keep the SDK forwarding code unchanged. Write the SHA-256 and absolute path of each instrumented file to `/symbols/harmony-instrumented-events`. Keep unstripped binaries under `/symbols`, each with a nonempty `*.sym.tsv` from the language's own tool.

Run `harmony preflight --image IMAGE` before booting a VM. For each rejected instruction, disassemble the reported executable at the reported address. Accept a site only when the runtime cannot reach it under Harmony's fixed CPUID. Record the executable digest, address, instruction, and reason in `/etc/harmony/instruction-allowlist`, as `workloads/languages/reviewed/` does. A rebuilt binary has a new digest and needs a new review.

Acceptance needs four results:

- Twenty ordered timer markers.
- Identical serial logs and run records from two boots with the same seed.
- Progress on one thread while the park launcher holds another.
- An event-enabled search on a reference case that reaches its oracle.

`workloads/languages/run-check.sh` runs the first three. Write the language reference from the recipe that passed, including its build inputs and known limits.

The service table follows Antithesis's Apache-2.0 [setup skill](https://github.com/antithesishq/antithesis-skills/tree/1fd8470d36a9629a75bda4619a5589d679a40d7c/antithesis-setup).

Finish preparation with:

```sh
harmony preflight --image IMAGE
```
