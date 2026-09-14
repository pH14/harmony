# Workload validation tools

These composition binaries exercise the platform with concrete NES and Postgres
workloads. Build with `cargo build --manifest-path workloads/tools/Cargo.toml
--release`. The execution core remains independently buildable; tool-specific
startup and evidence conventions live here.

The `kvm_x86_nova_probe` restore oracle accepts a kernel, platform runtime
initramfs, generic NES OCI image, and Nova ROM:

```sh
cargo run --release --manifest-path workloads/tools/Cargo.toml \
  --bin kvm_x86_nova_probe -- \
  bzImage initramfs-oci.cpio.gz nes.oci nova.nes
```

It prepares that image through `nes-workload`, constructs a
`consonance-client::Session`, and reads the kernel-owned observation through
`Session::read_observation`. It builds a 50-action branching snapshot tree and
checks 200 restored continuations, requiring zero fresh-VM fallbacks. The
backend README documents the generic KVM restoration invariant; this
workload-specific validation remains with its tool.
