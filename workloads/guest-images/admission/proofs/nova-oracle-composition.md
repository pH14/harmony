# Reviewed Nova A–E controlled composition

Proposed published-platform refresh; pending explicit primary acceptance. This extends the reviewed NES guest to the named nova-ae-linux-x86_64-kvm-oracle-v1 configuration; it does not admit arbitrary oracle configurations.

An independent comparison against the accepted default NES dump finds all guest input hashes, prepared execution identity and composition offsets identical. Only session-config.json changes, plus explicit host-oracle metadata. The fixed kernel, platform, NES executable/loader closure and ROM therefore retain their separate component reviews.

The actual boot path and admission dump share the RAM, seed and command-line constructor: 128 MiB, seed 0x4e4f56415f434931, required noxsaveopt/noxsaves/LD_BIND_NOW flags, and deferred checkpoint hashing. The direct ControlServer uses in-place restoration with a remap factory, sixteen [0,1] setup payloads, fixed default tree seed and absolute virtual-time deadline 2000000000. Source review confirms those constants are shared with execution. Tree-seed overrides are rejected for this admission.

The guest composition review digest is 89f6addc0f8d8d18de25e908070dee0c9ebfea9eaae8571527b2875acd72d5b2. It excludes only host source/executable hashes; these remain bound in each full candidate and verification checks the actual executable. Host source changes still require normal PR review and runtime qualification; a matching guest composition is not approval of arbitrary host behavior.

Scope remains trusted fixed guest code, the pinned Nova ROM, normal instruction entry, and no generated/JIT code, writable executable mappings, arbitrary modules or code mutation. The writable root is not runtime immutability enforcement. Successful guest-initiated comparison endpoints are covered; arbitrary snapshot imports, snapshots after failed runs, external NMI/MCE injection and unhealthy memory are not established by this review.

CI must generate its candidate from the exact inputs it runs, verify immediately before execution with the same executable, preserve restore-oracle mode and absent tree override, and retain source/artifact/run provenance. New kernel/platform/workload bytes must fail this baseline pending review; the in-progress publisher is not assumed byte-identical.

Published fef80533 proposal: archive `733555f781a62f5a2e4d37ae9297efe1c1a347c395586e2cc24b1e38ddf353e3`. Only platform.cpio.gz
and composed-initramfs.bin file hashes/sizes change from the prior composition;
prepared identity, rootfs/control bytes, session configuration, workload code
and ROM/SQL inputs remain unchanged. The shared platform-component.json now
references the fresh supervisor review. Pending primary acceptance; no
approval is inferred merely from publisher success.
