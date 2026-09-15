# Fixed PostgreSQL image loader review (2026-09-14)

Conditional compatibility evidence, not approval. Read-only audit of ms02 image `/tmp/harmony-pg-g1-build-20260914/pg-root`. No image bytes or host configuration changed.

Loader `lib64/ld-linux-x86-64.so.2`: SHA256 `c8438e4fde1934e61c88311633f00949ff645d5c04cdb8671fa3d78164d2f307`; build ID `86f7eb4ff495ed3f2831c6d3d662e4321762a2a3`. Matching build-ID debug file in existing loader-review artifact identifies function sizes. Debug package metadata: glibc 2.41-12+deb13u4. Debian exact-version source archives retrieved from https://deb.debian.org/debian/pool/main/g/glibc/ and all `debian/patches/series` applied locally with git apply. Archive SHA256s match source manifest: upstream `f24aa441021121a79266f0d75242706cab8843a47901fefe74527491807f1998`; Debian `dda4153511bfd543502d18e5bc9323110996fe9c531f14ae3fad6eb17f027c7d`. This establishes package/source correspondence and instruction-level agreement, not a reproducible rebuild.

## Proposed symbol-sized regions

Half-open offsets; file offsets equal ELF virtual addresses in executable PT_LOAD (offset/vaddr 0x1000, filesz 0x273d1). Padding excluded.

| Symbol | Start | End | Save instruction | SHA256 of region |
|---|---|---|---|---|
| _dl_runtime_resolve_xsave | 0x133c0 | 0x13489 | 0x13438 | 1729a188b522faeb2c1f9f5f0cd7b89b832214d179897c8cda3d2afbeb4e4e2a |
| _dl_runtime_resolve_xsavec | 0x13490 | 0x13549 | 0x134f8 | 8c30ae40b76cfcbca10f1d8ae877c28ea7dbd0dadc4d8a96281471b4fe708731 |
| _dl_tlsdesc_dynamic_xsave | 0x17f70 | 0x18076 | 0x18030 | cdf6367cfa1aa71d1d639fde3cce5b625b9ac4a66b307b2f48413fe0ff5c42ee |
| _dl_tlsdesc_dynamic_xsavec | 0x18080 | 0x18176 | 0x18130 | 481c7bcfd3eeb4fac8a013d0afe5781b495a6a396d53dd662fa554e68fef09e8 |

## Incoming-reference and source reasoning

CPU feature initialization takes resolver entry addresses at 0x17177/0x17192 (PLT) and 0x1717e/0x17199 (TLSDESC), storing selected pointers at 0x35d48 and 0x35d40 respectively. These are the entry-address references in the existing full disassembly; their existence is not execution. Patched `sysdeps/x86/cpu-features.c:1281-1293` agrees.

PLT selected-pointer consumer is 0xe9b9. `sysdeps/x86_64/dl-machine.h:78-126` installs GOT[2] only when DT_JMPREL and lazy are set. `elf/rtld.c:2653-2655` turns nonempty LD_BIND_NOW into dl_lazy=0; startup relocations use it at 2269/2294. `elf/dl-open.c:643-646` also suppresses RTLD_LAZY on subsequent dlopen when dl_lazy is zero. Profiling can force lazy at rtld.c:2258, so its absence is material. Under the stated startup environment and no loader overrides, normal ABI PLT resolution does not enter these save regions.

TLSDESC selected-pointer consumers are 0x10519, 0x10880, 0x10b83: each installs the pointer in a descriptor entry after creating the dynamic argument. `dl-machine.h:362-394` does this for R_X86_64_TLSDESC when static TLS allocation cannot satisfy it. This applies during eager relocation too: LD_BIND_NOW alone does NOT exclude these functions. `dl-tlsdesc-dynamic.h` fast path tests DTV generation/allocation; stale generation or unallocated TLS branches to save sites, then calls __tls_get_addr_internal. Actual branch targets 0x17fb9/0x180c9 and masks 0xe00ef agree with source.

Independent exclusion for TLSDESC: supplied relocation inventory `/root/harmony-pg-g1-image-audit-tlsdesc.json`, SHA256 `33a5db9f3a8c180dfb12a88a3a2149dba1f1fa099821490a2e8f36cf9442ff58`, records 152 ELFs with per-file hashes and zero TLSDESC relocations. With that exact complete runtime closure, ordinary loader relocation creates no dynamic TLSDESC entries and thus no ABI descriptor calls enter either region. Ordinary TLS calls to __tls_get_addr are not calls to these wrappers.

## Limits and remaining policy work

The four entries are supported as conditional normal-execution exclusions, not universal unreachability or automatic admission. They require enforcement of nonempty LD_BIND_NOW at every exec, no profiling/auditing/preloading/tunables or alternate loader inputs, exact inventoried runtime code, and fixed admitted SQL/workload with JIT off. This review does not independently prove that startup environment and runtime module closure are enforced, nor that arbitrary direct/indirect control transfers, memory corruption, generated code or future SQL/extensions cannot enter these bytes. Absence of direct disassembly references alone is not such proof. Parent-provided 297-edge completeness and workload constraints are assumptions, not revalidated here. Region digests must be tied to loader/image and policy digests; any change invalidates the conditional conclusion.
