# OCI support

`oci-support` acquires registry images, OCI layouts, and Docker archives; stages
their rootfs; and assembles deterministic guest image segments. Image parsing,
layer validation, and optional digest-keyed caching are shared by the Harmony
OCI callers. The host preparation API is `bundle::prepare`:

```rust
let staged = image::stage(image_name, staging_dir)?;
let request = LaunchRequest::new(command)
    .with_bundle("/etc/harmony/bundle")
    .with_external_inputs(vec![ExternalInput::new("/etc/input", bytes)]);
let prepared = bundle::prepare(&staged, &request)?;
let initramfs = prepared.initramfs(platform_initramfs);
```

`PreparedExecution` owns the deterministic rootfs and platform control
segments, the resolved `ExecutionSpec`, and a SHA-256 identity. The rootfs is
placed at `/harmony-oci/rootfs`; the control segment owns
`/harmony-oci/config.json`, `/harmony-oci/execution.json`, and sorted external
files. Image contents cannot replace those control paths. The canonical OCI
configuration always starts `/usr/lib/harmony/supervisor` as root. The
supervisor reads the mounted execution specification and applies its argv,
environment, working directory, and resolved image credentials to the
application process.

External destinations are absolute normalized paths with bounded count and
payload size. Platform-owned paths, duplicate destinations, and file/parent
conflicts are rejected. External files and the execution specification are
read-only mounts; `/dev/harmony` and the supervisor-only `/dev/harmony-park`
are the only Harmony device mounts. Guest startup invokes the pinned
`/usr/bin/runc` once with the initramfs `--no-pivot` arrangement.

`bundle::build_rootfs_segment` places an image under `/harmony-oci/rootfs`.
`bundle::build_control_segment` is available when a caller already has an
`ExecutionSpec`; most callers should use `bundle::prepare`. Other packages
append their own platform segment using `guest-image::Writer`.

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

`RuntimeConfig::user` retains the image `Config.User` value. The supported
Linux forms are `uid`, `uid:gid`, `user`, `user:group`, `uid:group`, and
`user:gid`. Numeric IDs are used as supplied. A named user or group must be
present in the staged rootfs account files, and an unknown named account is an
error. A numeric user without a group uses its staged `/etc/passwd` primary
group when that UID is present; an unknown numeric UID keeps that UID and uses
GID 0, so it never becomes UID 0. An omitted or empty user means root.

`image::resolve_process_credentials` resolves this value against only
`rootfs/etc/passwd` and `rootfs/etc/group`. If the group is omitted, the
user's primary GID comes from `/etc/passwd` and memberships listed in
`/etc/group` become sorted, de-duplicated `additional_gids`, excluding the
primary GID. An explicit group selects only that GID and suppresses all
supplementary groups. A missing optional group file contributes no additional
groups; malformed or out-of-rootfs account files fail resolution. Named users
never fall back to root.

Runtime environment entries must use `VARNAME=VARVALUE` with a shell-safe
variable name. Invalid entries fail image parsing instead of being silently
discarded by the guest launcher.

```sh
cargo test --manifest-path consonance/oci/Cargo.toml
cargo clippy --manifest-path consonance/oci/Cargo.toml --all-targets -- -D warnings
```
