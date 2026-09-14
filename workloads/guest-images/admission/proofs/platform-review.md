# Final standard-platform CPIO candidate review

Archive SHA256: `44e520bf7db067310d49362a566da2cf36ead6df9867b1ee92cc948ed16849b0`.
Source archive SHA256: `6ace7267bd480d535d908c06571c5322625b1744ca46072841190dc352d7eee1`,
retained under the b51ebb8d lab build. See source-manifest.txt and both build
manifests. Kernel SHA256: `7ce25244cf1d138db1286ce61fb1c880bd224b2866ffaebd1ec19d04b2ad69b6`.
This is the standard-platform candidate; the full builder/task-park qualification
was still pending the header-directory fix when reviewed. No approval is inferred
from the standard kernel/OCI gate.

The exact archive contains60entries, three ELF files and char devices
/dev/console5:1, /dev/kmsg1:11, /dev/null1:3. Devices were parsed as metadata,
never created/opened on the host. Kernel gen_init_cpio terminates symlink bodies
with NUL; the adapter now accepts one optional finalNUL, rejects embeddedNUL,
and binds raw target bytes. All32 GNU tests pass after this format fix.

| ELF | SHA256 | Remaining instruction proof |
|---|---|---|
| /bin/busybox | `ed276986a91b1c13f6da78900446dfa4311e841d87d8f770ae3ca10b18767b70` | Two exact XSAVE/XSAVEC resolver regions plus ECX0 and final startup-binding scope |
| /usr/bin/runc | `ce6353a8273004c5f917277846dd521c7185b653ca46cfe538cc16b1be254cc9` | One ECX0 XGETBV control-flow proof; no save-region exception |
| /usr/lib/harmony/supervisor | `e0ef13ac2e0d37cf0779fc55bb3a6b173e5004d4f27ca6fc5b90b4d9f2488cca` | One ECX0 XGETBV control-flow proof; no save-region exception |

All three have no PT_INTERP/NEEDED, text relocations, executable stack, or W+X
PT_LOAD segments. The candidate remains admitted=false. Actual finalarchive
metadata, source/build identity and per-ELF instruction sequences are retained
in final-standard-platform.json and adjacent manifests.

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
