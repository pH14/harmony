# OCI workload support

`oci-support` acquires registry images, OCI layouts, and Docker archives; stages
their rootfs; and assembles deterministic guest image segments. Image parsing,
layer validation, and digest-keyed cache behavior are shared by the Harmony OCI
runner and workload packages. Packages own startup and checking contracts.

`bundle::build_rootfs_segment` places an image under `/harmony-oci/rootfs`.
`bundle::build_control_segment` provides the existing OCI command runner.
Other packages append their own launch segment using `guest-image::Writer`.

```sh
cargo test --manifest-path workloads/oci/Cargo.toml
cargo clippy --manifest-path workloads/oci/Cargo.toml --all-targets -- -D warnings
```
