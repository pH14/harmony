# nes-busybox: artifact review

ELF SHA256 dd40538865c749943b671e11ef9f644f86dd6037abe35dcbbf7f7d67853ae3ee.
Review covers the exact static ELF, complete executable PT_LOAD scan, absence
of W+X segments/executable GNU_STACK, sole ECX0 XGETBV proof, and the two exact
resolver regions below. FXSAVE/FXRSTOR/XRSTOR are inventoried but this policy
forbids XSAVE-family saves. Incoming-reference and eager-binding arguments are
in the associated named proof files. Conditional controlled-scope obligations
must be fulfilled in the final composition before using this proposed entry.
