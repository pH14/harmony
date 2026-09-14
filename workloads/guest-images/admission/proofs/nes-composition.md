# Proposed nes prepared composition review

Candidate only; explicit reviewer acceptance remains required. Manifest SHA256 7a4fd4989d542de1673d9d13f6ea7b8177a763e80b85bb950b126aec58beb8c4; prepared identity 23e316ba5fbc7ca2f55dab7756fd2f82912d27b0b8960bb0cfd559c666f22f6f. Exact metadata is retained in nes-prepared/.

The native Linux helper uses actual OCI staging and preparation APIs, serialized SessionConfig::default(), and ordered platform/rootfs/control concatenation. Component parsing rejects unsupported collisions; this is not general Linux overlay emulation. The session, kernel, OCI configuration/input files, argv/environment, external ROM or SQL/script code inputs and composition offsets are bound by the manifest. Required pre-PID1 and every-exec LD_BIND_NOW scope is checked separately from instruction review.

Scope is the exact default-session controlled workload. Custom campaign/fault sessions, other argv, SQL, ROMs, dynamic code, writable executable memory and code mutation are outside this proposal. The rootfs remains writable; static inventory is not runtime immutability enforcement. Trusted normal execution and no interior indirect entry are explicit review assumptions. PostgreSQL's absence of active psqlrc and fixed environment is recorded in ../pg-startup-files.md; NES ROM matches current nova-versions.env output pin.
