# nes-play-agent: artifact review

ELF SHA256 4cc1dacf6ef2aed5638e5eb6eceb7130eca532ed351e3a5b692da0a903a6349a.
Review covers the exact static ELF, complete executable PT_LOAD scan, absence
of W+X segments/executable GNU_STACK, sole ECX0 XGETBV proof, and the two exact
resolver regions below. FXSAVE/FXRSTOR/XRSTOR are inventoried but this policy
forbids XSAVE-family saves. Incoming-reference and eager-binding arguments are
in the associated named proof files. Conditional controlled-scope obligations
must be fulfilled in the final composition before using this proposed entry.
