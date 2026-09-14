# Exact PostgreSQL TLS descriptor closure review

The retained harmony-pg-g1-image-audit-tlsdesc.json inventory has152ELFs and zero
TLSdesc relocations. Its complete path/SHA256 map was compared exactly against
the actual Rust-prepared workload-root candidate (not just launch dependencies):
all152match, with297resolved direct edges. The independent loader-review.md
identifies the two _dl_tlsdesc_dynamic_xsave/xsavec regions and their installation
only through TLSdesc relocations. Thus this fixed normal loading closure creates
no TLSdesc entry pointing to either wrapper. Eager binding is NOT this proof.

Unchanged shipped dlopen modules are included. No extra modules, generated code,
interior function-pointer calls, mutable-code substitution or untrusted SQL is
supported. Any changed rootfs/ELF/loading policy invalidates this conclusion.
Final reviewer acceptance of those trusted-scope assumptions remains required.
