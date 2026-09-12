# Workload validation tools

These composition binaries exercise the platform with concrete NES and Postgres
workloads. Build with `cargo build --manifest-path workloads/tools/Cargo.toml
--release`. The execution core remains independently buildable; tool-specific
startup and evidence conventions live here.

The `kvm_x86_nova_probe` restore oracle builds a 50-action branching snapshot
tree and checks 200 in-place restored continuations, requiring zero fresh-VM
fallbacks. The backend README documents the generic KVM restoration invariant;
this workload-specific validation remains with its tool.
