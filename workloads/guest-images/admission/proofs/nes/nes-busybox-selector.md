# nes-busybox: selector review

ELF SHA256 dd40538865c749943b671e11ef9f644f86dd6037abe35dcbbf7f7d67853ae3ee.
The sole XGETBV at 0x4a5c99 is in update_active.constprop.0.
nes-busybox-selector.json records the seven-instruction decoded chain: ECX zeroing
followed by instructions that preserve ECX. The bounded scanner observed no
direct incoming edge into this chain after zeroing. CPU-feature startup calls
the containing function normally; the reviewed trusted program does not form
an indirect interior pointer to these instructions. This is a digest-specific
control-flow conclusion under controlled-scope.md, not an exemption for an
unproved selector. No XGETBV1 invocation is admitted.
