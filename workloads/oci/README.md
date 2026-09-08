# OCI workload support

`oci-support` acquires registry images, OCI layouts, and Docker archives; stages
their rootfs; and assembles deterministic guest image segments. Image parsing,
layer validation, and digest-keyed cache behavior are shared by the Harmony OCI
runner and workload packages. Packages own startup and checking contracts.

`bundle::build_rootfs_segment` places an image under `/harmony-oci/rootfs`.
`bundle::build_control_segment` provides the existing OCI command runner.
Other packages append their own launch segment using `guest-image::Writer`.

Layers are extracted with `tar -p --no-same-owner`: each entry takes the mode
its header recorded, whatever the host umask, and belongs to the staging user
even when that user is root. The owner each header recorded is kept in
`image::Ownership` beside the tree rather than on it. Whiteouts and replaced
entries update the map the way they update the tree, and the rootfs segment
records the mapped owner in every entry, which the guest kernel applies when it
unpacks the initramfs. An image that hands its data to a service account, such
as a database cluster owned by its own uid, is therefore readable by that
account in the guest and by nobody else.

A layer that ships a child without an entry for its parent directory leaves
that parent with the mode `tar` gave it when it created it, and the merge
applies that mode over the lower layer's.

```sh
cargo test --manifest-path workloads/oci/Cargo.toml
cargo clippy --manifest-path workloads/oci/Cargo.toml --all-targets -- -D warnings
```
