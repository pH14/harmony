---
name: preparing-workloads
description: Prepare a service or language workload image for deterministic Harmony execution, including instrumentation, symbols, reviewed instructions, and image admission. Use for new workload recipes or rejected image preparation; ordinary host setup is outside this skill.
---

# Preparing workloads

Inventory every service before changing its image:

| Service | Language and version | Build system | Runtime artifact path | Symbol files |
| --- | --- | --- | --- | --- |

Include helper processes and native libraries that can run long loops. Identify the service's source revision, startup command, readiness check, and existing Harmony bundle. Preserve the user's workload and deployment choices.

Every target needs three properties: hidden instructions occur only at reviewed sites; the runtime does not generate executable code; and every application loop reaches a libvoidstar callback. Static ELF admission checks instructions and writable executable file segments and executable stacks. It cannot prove arbitrary code has no dynamic code generation: verify runtime/build settings and relevant memory mappings in the language recipe. The guest detector for approved JIT runtimes is a separate, conditional stage.

Read [the compiled-language reference](references/compiled.md) for C, C++, LLVM front ends, Rust, and GCC. Only use a language reference after its recipe has passed its acceptance checks.

Install the composed runtime at `/usr/lib/libvoidstar.so`. Use the shared build in `workloads/languages/build-runtime.sh`; language layers load this library at execution time. Keep the SDK forwarding code unchanged. Write SHA-256 and absolute image paths to `/symbols/harmony-instrumented-events`, preserve unstripped binaries under `/symbols`, and write a nonempty `*.sym.tsv` using the language's own tool.

Run image preflight before a VM test. For each rejected instruction, inspect the exact executable and disassembly at the reported module address. Accept a site only after confirming the runtime cannot execute it under Harmony's fixed CPU policy or that it uses an audited deterministic mechanism. Record the executable digest, address, instruction, and concrete rationale in `/etc/harmony/instruction-allowlist`. Do not copy approvals across changed binaries or approve all instructions merely to pass admission. Remove unused diagnostic libraries instead of approving their counter instructions.

Require ordered timer markers, identical logs and execution records from two fixed-seed boots, progress by another thread while one is parked, and an event-enabled reference-case search. Use `workloads/languages/run-check.sh` for the common fixture checks. Write the language reference from the tested recipe, including build inputs and observed limitations.

The inventory structure follows Antithesis's Apache-2.0 [setup skill](https://github.com/antithesishq/antithesis-skills/tree/1fd8470d36a9629a75bda4619a5589d679a40d7c/antithesis-setup). Harmony's image rules and acceptance commands are maintained here.

Finish preparation with:

```sh
harmony preflight --image IMAGE
```
