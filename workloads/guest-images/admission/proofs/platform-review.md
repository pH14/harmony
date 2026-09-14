# Reviewed OCI platform, e2a9ac58

Accepted by the Codex primary agent after exact source verification, a fresh whole-archive GNU instruction/dependency scan, and the new supervisor selector/incoming-edge review. Source key: 8d0affa9201175cdc8abd1c51594dfa060463edb053b6f6ba43e2a001cae3fdb. Kernel remains 7ce25244cf1d138db1286ce61fb1c880bd224b2866ffaebd1ec19d04b2ad69b6.

The OCI archive ee6b601b9f3dc04f1547fbe5394df62933dd20e1b57139a1c0fe3d6cd40f3f70 changes only the supervisor entry. Its code changes materially; no whole-binary equivalence is claimed. The fresh scan finds three ELFs, no new dependency, writable/executable segment, executable stack or text relocation. The new supervisor has only the ECX-zero XGETBV wrapper at 0x6ce90, with instruction at 0x6ce92 and direct caller at 0xe16c. Exact selector bytes and incoming-edge evidence are retained in platform-selectors.md and platform-publisher-e2a9ac58/.

BusyBox and runc have unchanged file hashes, so their exact previous resolver/selector evidence remains applicable. Main's structured-bundle launch/recovery paths are outside the admitted bundle:null sessions; their dispatch and ordinary execution environment path were reviewed in source. The changed libvoidstar in the separate direct initramfs is not covered by this OCI component approval. No arbitrary indirect entry, generated code, mutation or runtime filesystem immutability is established.

## Fresh BusyBox resolver review

This is a different GNU BusyBox digest from prior NES/platform reviews; no old
region was reused. The final binary's executable PT_LOAD addresses and hashes
match its new unstripped companion (companion-binding.json). Fresh symbols,
references and machine-code context are retained.

XSAVE wrapper: start0x4dab10,size0xcd,SHA256
735459c051cb69af8a1cb6ed7d6675dd0ab1cef4f5ea5b07b5a1dfbc0fd68ef6.
XSAVEC wrapper: start0x4dabe0,size0xbd,SHA256
219a7e5e3e2cc5b936dd699bf5da9d9ed67a6bdb355d03723e3346eb1fa35b0a.
Save instructions are0x4dab8c and0x4dac4c respectively.

CPU-feature initialization takes resolver addresses at0x47321b/0x473231/
0x47346a and writes the chosen pointer at0x473238. Its observed consumer at
0x49c1d2 is the lazy GOT setup. The startup write to _dl_lazy at0x48b0bf follows
getenv of the exact string LD_BIND_NOW at0x538776 and stores zero for a nonempty
value. dl_open_worker_begin reads that global at0x46cabd; when zero it clears
the RTLD_LAZY bit at0x46cad2 before relocation. This fresh machine code supports
ordinary lazy-GOT exclusion with LD_BIND_NOW=1 before startup, including static
dlopen. It does not rely on old GNU addresses/libc patch identity or static
linkage alone. Profiling/auditing, arbitrary function-pointer calls and code
mutation remain excluded controlled-scope assumptions.

The sole BusyBox XGETBV at0x472762 has a scanner-proven ECX0 chain recorded in
the archive inventory. Runc's sole site is0x4058e5; supervisor's is0x62ca2.
Their exact final digests differ from prior artifacts and require matching
control-flow evidence in the final baseline. No generic digest/name exemption
is emitted. Final runtime/init environment, no-code-mutation scope and platform
qualification must be closed before proposed region arguments become approval.
