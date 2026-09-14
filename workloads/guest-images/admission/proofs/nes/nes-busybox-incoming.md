# nes-busybox: exact resolver incoming-reference review

ELF SHA256 dd40538865c749943b671e11ef9f644f86dd6037abe35dcbbf7f7d67853ae3ee.
Reviewed symbol ranges and disassembly references are retained in
nes-busybox-symbols.txt and nes-busybox-references.txt. BusyBox symbols come from the
unstripped build companion whose executable PT_LOAD bytes/addresses were
matched to the shipped binary in D4; agent symbols are in the shipped ELF.

The observed resolver addresses are taken only by CPU-feature initialization
at 0x4a6780/0x4a6789/0x4a6799; the selected function pointer is written at
0x4a67a0. Its observed consumer at 0x450e35 installs the ordinary
lazy GOT entry under the lazy-relocation branch. No direct call/jump to these
resolver functions was found in full disassembly. No resolver-target relocation
was found. Literal 64-bit pointer search found none for BusyBox and only
nonallocated .symtab entries for the agent. These checks complement the reviewed
static glibc2.41 source path; they are not exhaustive arbitrary-code reachability
analysis or protection against corrupted function pointers.

Within the controlled trusted program, these entries are reached through the
loader's lazy GOT mechanism. Eager binding proof in nes-busybox-binding.md excludes
its installation. No arbitrary dlsym/integer-to-function-pointer invocation,
profile/audit mode, code mutation, or generated-code path is admitted. All
retained instruction exceptions are exact whole-function byte regions from
regions.json, never a generic symbol-name exemption.
