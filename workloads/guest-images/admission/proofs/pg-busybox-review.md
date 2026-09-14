# Exact PostgreSQL BusyBox resolver review

ELF dfcabe58212e7b2d8cabeac17a779f8572a4f97836bbcc35c3c45a98d28f1de0.
The companion executable PT_LOAD hashes match exactly (pg-busybox-regions.json).
Fresh function regions are xsave0x4ea0b0+0xc9 and xsavec0x4ea180+0xb9, with byte
hashes in that file. CPU features take addresses0x4b6340/0x4b6349/0x4b6359 and
store the selected pointer0x4b6360. Its observed lazy-GOT consumer is0x455cf5.
Static startup writes _dl_lazy at0x481a8b; dlopen reads it at0x460328. These are
fresh digest-specific references, not reused NES offsets. The reviewed Debian
glibc2.41 static initialization sets _dl_lazy=0 from nonempty startupLD_BIND_NOW;
ordinary dlopen then clears RTLD_LAZY. The exact fixed PG shell preserves that
environment (pg-launch-scope-review.md), excluding ordinary entry through the
lazy GOT. Profiling/auditing, arbitrary interior calls and code mutation are
excluded trusted-scope assumptions. Final acceptance remains pending reviewer.
