# Proposed published-platform refresh, fef80533

Pending primary acceptance. Published run34865561695 artifact archive SHA256
`733555f781a62f5a2e4d37ae9297efe1c1a347c395586e2cc24b1e38ddf353e3`; rootfs digest `1a757c74f9cf2d930387f928511f1186742febe0639e17414f2b79c52316b532`. Kernel remains
`7ce25244cf1d138db1286ce61fb1c880bd224b2866ffaebd1ec19d04b2ad69b6`.
The source-bound runtime manifest verifies exact-input for source key
`8e6c72124f57bd02490811e18c80f806326de525e1388c2ad25edb7d9f942683`.
The bridge binds the payload and Nix artifacts to that source, with
nightly-2026-06-16, target x86_64-unknown-linux-musl.

The parsed archive differs from the prior review only in supervisor file
size/hash:879856→879848 bytes, `f5dd3b5dcf3f3fcf19ae1fa2c92eefe981a32ab65dd2195d210425a7b3fa5458`. All other
entries, permissions, ownership, devices, scripts and symlinks are unchanged.
BusyBox and runc retain exact prior hashes and region/selector proofs.
The3-ELF scan adds no save instruction, dependency, WX segment, executable
stack, or text relocation. Supervisor's only relevant site remains XGETBV0.

Supervisor .text address/size and decoded instruction boundaries are unchanged.
Of128390 decoded instructions,4527 have changed bytes; every change is a
same-mnemonic RIP-relative memory operand. Direct control-transfer bytes are
unchanged. Diagnostic dependency paths change /root/.cargo to
/home/runner/.cargo, .rodata grows64 bytes, and linker symbols/relocations
change correspondingly. This is not a claim of bit-identical .text.
No supervisor/runtime/local-dependency source diff exists from b51ebb8d to
fef80533. Compiler comments match rustc1.98.0-nightly01dfd7924(2026-06-15).
The exact ECX0 wrapper is separately established by platform-selectors.md.

Raw publisher provenance and bounded machine/source evidence are in
platform-publisher-fef80533/. Their file hashes are retained in SHA256SUMS.
This refresh remains a proposal until explicit primary acceptance; prior
approved history should remain in Git. No general runtime immutability or
arbitrary-code admission is claimed.

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
