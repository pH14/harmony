# x86 guest CPU policy

`guest.toml` defines the single x86 machine presented to a Harmony guest on all
physical hosts. It fixes CPUID values, MSR access rules, modeled devices, and
virtual-time durations. The `GenuineIntel` vendor string and family/model fields
are guest-visible compatibility values, not a requirement to run on Intel or a
particular processor. There are no per-vendor or per-microarchitecture files,
CPU identity probes, or microcode pins.

`vmm-core` embeds this file and derives the CPUID model, MSR filter, access
handlers, timing rules, and snapshot contract hash from it. Backend capability
requirements still apply: changing a host cannot supply a missing hypervisor
feature or emulate an unsupported native instruction. The cooperative guest
uses the advertised instruction set. Hidden, untrappable instructions are
outside that surface; their physical absence is not assumed. See
[Determinism](../../../../docs/DETERMINISM.md).

Stock-KVM virtual-time composition additionally hides hardware RNG feature bits
because that backend cannot trap RDRAND/RDSEED. This is a backend capability
choice shared by all hosts, not a processor-specific contract.

## Changes and compatibility

Policy version 6 removes the host identity and physical-absence assertions from
the canonical form. It retains guest microcode/CR4 invariants and versions the
cooperative instruction boundary. The contract hash changes, so snapshots from
older policy versions are rejected before restore mutates the VM. Guest CPUID
values, MSR dispositions, and virtual-time durations are unchanged.

Changes to guest semantics require a version bump, regenerated canonical bytes,
and an updated committed `contract_hash`. Run:

```sh
cargo test -p vmm-core contract::tests
cargo test -p vmm-core contract::tests::regen_golden -- --ignored
cargo test -p vmm-core contract::tests::report_contract_hash -- --nocapture
```

Review the generated golden and commit its hash in `guest.toml` and the golden
regression test. The old Coffee Lake captures and unused AMD draft remain in Git
history, not in runtime policy or the test matrix.
