# Proposed postgres prepared composition review

Candidate only; explicit reviewer acceptance remains required. Manifest SHA256 769b2cc0573bd9a29f0def74919324dfec555b6969f99d3bb4105053d0c580b3; prepared identity 28b0d9c068cca80dc64aea44991cdd3740647eade4b2ce3162325910014c8697. Exact metadata is retained in postgres-prepared/.

The native Linux helper uses actual OCI staging and preparation APIs, serialized SessionConfig::default(), and ordered platform/rootfs/control concatenation. Component parsing rejects unsupported collisions; this is not general Linux overlay emulation. The session, kernel, OCI configuration/input files, argv/environment, external ROM or SQL/script code inputs and composition offsets are bound by the manifest. Required pre-PID1 and every-exec LD_BIND_NOW scope is checked separately from instruction review.

Scope is the exact default-session controlled workload. Custom campaign/fault sessions, other argv, SQL, ROMs, dynamic code, writable executable memory and code mutation are outside this proposal. The rootfs remains writable; static inventory is not runtime immutability enforcement. Trusted normal execution and no interior indirect entry are explicit review assumptions. PostgreSQL's absence of active psqlrc and fixed environment is recorded in ../pg-startup-files.md; NES ROM matches current nova-versions.env output pin.

Published fef80533 proposal: archive `733555f781a62f5a2e4d37ae9297efe1c1a347c395586e2cc24b1e38ddf353e3`. Only platform.cpio.gz
and composed-initramfs.bin file hashes/sizes change from the prior composition;
prepared identity, rootfs/control bytes, session configuration, workload code
and ROM/SQL inputs remain unchanged. The shared platform-component.json now
references the fresh supervisor review. Pending primary acceptance; no
approval is inferred merely from publisher success.
