# Antithesis Go instrumentation evaluation

We evaluated the public [Antithesis Go SDK](https://github.com/antithesishq/antithesis-sdk-go)
at commit `90a0e77918f069b745404d904b72c9417fd43ac5`, the revision used for this assessment.
The repository is MIT licensed. Harmony does not vendor or modify it yet; any future vendored
copy must retain the upstream `LICENSE` and copyright notice.

The SDK has two relevant pieces:

- `antithesis-go-instrumentor` rewrites Go source or ASTs and registers edge coverage modules.
- The runtime reports coverage through the `libvoidstar` ABI, with an optional lease API that
  suppresses repeat notifications until the host revokes a lease.

This is useful coverage plumbing, but it is not a Harmony scheduling adapter. The SDK's CGo path
expects the Antithesis runtime ABI, while the no-CGo path is a local no-op/random implementation;
neither path emits Harmony `Moment` choices or guest-kernel scheduling events. Instrumenting etcd
therefore cannot, by itself, make Consonance explore a reproducible schedule.

The integration boundary we should implement if this evaluation proceeds is a small Harmony
runtime package for the cooperative guest kernel:

1. preserve the instrumentor's edge/module catalog in the workload image;
2. map coverage callbacks to Consonance observations through a documented ABI, without making
   correctness depend on whether the Antithesis runtime is present;
3. let the cooperative kernel force deterministic exits at the same safe points on every host;
4. keep the existing process-fault and client-oracle contract as the correctness authority.

Until that adapter exists, the etcd museum case intentionally uses the existing fault-agent
process lifecycle and a client journal oracle. It has no coverage, timing, or runtime knobs that
can change correctness or portability. Antithesis instrumentation remains an optional capability
for search guidance and diagnostics rather than a prerequisite for a valid result.
