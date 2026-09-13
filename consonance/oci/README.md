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
conflicts are rejected. Platform and external mount destinations must have
symlink-free paths in the staged image, so image aliases cannot redirect a
mount over platform control files. External files and the execution specification are
read-only mounts; `/dev/harmony` and the supervisor-only `/dev/harmony-park`
are the only Harmony device mounts. The container also receives the guest
kernel's `/dev/kmsg` log observer as a read-only bind with read-only device
policy access. Guest startup invokes the pinned
`/usr/bin/runc` once with the initramfs `--no-pivot` arrangement. The outer
guest root is made recursively private before launch; the container uses
`rslave` propagation, as required by this runtime mode.
The generated device policy contains four numeric placeholders for the two
kernel-created Harmony character devices and a fixed read-only rule for
`/dev/kmsg` (character major 1, minor 11). Platform PID 1 resolves the
placeholders before invoking `runc`, allowing read/write access to the exact
Harmony major/minor pairs while keeping kernel-log access read-only.
Each container has a private cgroup namespace with a writable cgroup v2 mount.
The supervisor first creates a delegated child below the cgroup holding the
device policy, then enters a new cgroup namespace rooted at that child and
remounts the view. It moves itself and its future children into a `runtime`
leaf, leaving the delegated root empty and enabling its available controllers.
Nested runtimes can manage sibling subtrees without accessing the guest's
outer cgroup hierarchy or widening the parent device policy.

Rootfs and control-segment assembly are internal to `bundle::prepare`, which
validates their combined mount layout before producing executable bytes.

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

The platform integration smoke uses a platform-owned OCI fixture and this same
preparation API. It checks launch metadata and credentials, bounded observation
reads, SDK events, complete state hashes across repeated boots and restore,
seed-sensitive entropy, clean observation revocation, and a missing-input
negative control. Missing artifacts are errors when the ignored hardware test
is explicitly selected:

```sh
HARMONY_PLATFORM_KERNEL=/path/to/kernel \
HARMONY_PLATFORM_INITRAMFS=/path/to/initramfs-oci.cpio.gz \
HARMONY_PLATFORM_FIXTURE=/path/to/fixture-layout \
cargo test --locked --release -p oci-support --test platform -- --ignored --nocapture
```

The fixture layout is built by `consonance/harmony-linux/runtime-fixture/package.py`.
Kernel, runtime, and fixture must target the host architecture. The test currently
uses the Linux in-process session backend; compilation alone is not a hardware
qualification result.
Run these checks in release mode because replay hashes the complete guest memory.

The structured process smoke uses the same preparation path with an external
`/etc/harmony/bundle`, installs a standing process-window service through the
session service factory, and checks supervisor registers and console markers
for ready, hook completion, pause, kill, restart, and park. It also restores a
portable snapshot and requires identical continuation evidence:

```sh
HARMONY_PLATFORM_KERNEL=/path/to/kernel \
HARMONY_PLATFORM_INITRAMFS=/path/to/initramfs-oci.cpio.gz \
HARMONY_PLATFORM_FIXTURE=/path/to/fixture-layout \
cargo test --locked --release -p oci-support --test process_platform -- --ignored --nocapture
```
