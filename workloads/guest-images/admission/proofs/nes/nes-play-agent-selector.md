# nes-play-agent: selector review

ELF SHA256 4cc1dacf6ef2aed5638e5eb6eceb7130eca532ed351e3a5b692da0a903a6349a.
The sole XGETBV at 0x1847b9 is in update_active.constprop.0.
nes-play-agent-selector.json records the seven-instruction decoded chain: ECX zeroing
followed by instructions that preserve ECX. The bounded scanner observed no
direct incoming edge into this chain after zeroing. CPU-feature startup calls
the containing function normally; the reviewed trusted program does not form
an indirect interior pointer to these instructions. This is a digest-specific
control-flow conclusion under controlled-scope.md, not an exemption for an
unproved selector. No XGETBV1 invocation is admitted.
