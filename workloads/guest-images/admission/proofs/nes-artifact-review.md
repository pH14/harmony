# nes exact closed-artifact review proposal

The corresponding proposed component baseline enumerates every ELF path/digest
and direct/transitive dependency graph from the actual preparation output.
Reviewed candidate contains 2 ELFs. None has writable/executable
PT_LOAD, executable GNU_STACK, or text relocations. All save instructions are
covered only by the explicitly reviewed exact regions; all XGETBV sites require
the separate selector review. Artifacts without relevant instructions need no
instruction exception. Entire file/owner/mode/symlink tree is bound; this is not
an approval of future hashes. Trusted no-generated-code/no-code-mutation and
normal ABI loading assumptions remain explicit scope obligations.
