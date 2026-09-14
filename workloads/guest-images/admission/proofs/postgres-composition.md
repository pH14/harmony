# Proposed postgres prepared composition review

Candidate only; explicit reviewer acceptance remains required. Manifest SHA256 51a5b16377e24a341bac59d173703ea30396da67c445ffa3b6e5a5ea212e452c; prepared identity 28b0d9c068cca80dc64aea44991cdd3740647eade4b2ce3162325910014c8697. Exact metadata is retained in postgres-prepared/.

The native Linux helper uses actual OCI staging and preparation APIs, serialized SessionConfig::default(), and ordered platform/rootfs/control concatenation. Component parsing rejects unsupported collisions; this is not general Linux overlay emulation. The session, kernel, OCI configuration/input files, argv/environment, external ROM or SQL/script code inputs and composition offsets are bound by the manifest. Required pre-PID1 and every-exec LD_BIND_NOW scope is checked separately from instruction review.

Scope is the exact default-session controlled workload. Custom campaign/fault sessions, other argv, SQL, ROMs, dynamic code, writable executable memory and code mutation are outside this proposal. The rootfs remains writable; static inventory is not runtime immutability enforcement. Trusted normal execution and no interior indirect entry are explicit review assumptions. PostgreSQL's absence of active psqlrc and fixed environment is recorded in ../pg-startup-files.md; NES ROM matches current nova-versions.env output pin.
