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

Each `image::stage` call creates its own directory beneath the supplied staging
directory. Reusing that directory, including after a failed stage, cannot carry
files from an earlier image into the next guest rootfs or its identity. The
caller owns the staging directory and removes it when the prepared image is no
longer needed.
Outer archives and layers are copied into private staging before admission and
extraction, so replacing a source archive after its members are checked cannot
change the bytes extracted into the guest image.

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
mount over platform control files. External files and the execution
specification are read-only mounts; `/dev/harmony` is the only Harmony device
mount. The container also receives the guest kernel's `/dev/kmsg` log observer
as a read-only bind with read-only device policy access. Guest startup invokes
the pinned `/usr/bin/runc` once with the initramfs `--no-pivot` arrangement. The outer
guest root is made recursively private before launch; the container uses
`rslave` propagation, as required by this runtime mode.

`LaunchRequest::with_kvm()` explicitly adds the guest's `/dev/kvm` read/write
bind and its exact character-device rule (major 10, minor 232). Its destination
must be symlink-free. The generated control bytes bind this choice into the
prepared identity. This is used by the inner Consonance workload with the
separately qualified nested-host kernel and CPU contract.
The generated device policy contains two numeric placeholders for the
kernel-created Harmony character device and a fixed read-only rule for
`/dev/kmsg` (character major 1, minor 11). Platform PID 1 resolves the
placeholders before invoking `runc`, allowing read/write access to the exact
Harmony major/minor pair while keeping kernel-log access read-only.
Each container has a private cgroup namespace with a writable cgroup v2 mount.
The supervisor first creates a delegated child below the cgroup holding the
device policy, then enters a new cgroup namespace rooted at that child and
remounts the view. It moves itself and its future children into a `runtime`
leaf, leaving the delegated root empty and enabling its available controllers.
Nested runtimes can manage sibling subtrees without accessing the guest's
outer cgroup hierarchy or widening the parent device policy.

Rootfs and control-segment assembly are internal to `bundle::prepare`, which
validates their combined mount layout before producing executable bytes.
Both segments use a fixed gzip header and a pinned Rust DEFLATE encoder. The
prepared bytes and identity therefore do not depend on the host's `gzip`
implementation or version.

Layers are extracted with `tar -p --no-same-owner`: ordinary file and directory
modes come from the archive rather than the host umask, and files belong to the
staging user even when that user is root. The owner and mode each header
recorded are kept in `image::Ownership` beside the tree. This matters because
macOS may strip setuid and setgid bits during extraction even when Linux
retains them. Whiteouts and replaced entries update these maps the way they
update the tree. The rootfs segment records the mapped owner and mode in each
entry, which the guest kernel applies when it unpacks the initramfs. An image
that hands its data to a service account, such as a database cluster owned by
its own uid, is therefore readable by that account in the guest and by nobody
else.

Archive path components must be ASCII. Admission checks their case-folded
prefixes before extraction and rejects names that differ only by ASCII case,
including collisions between layers. This bounds staging to names whose byte
identity has the same meaning on case-sensitive Linux filesystems and the
case-insensitive, Unicode-normalizing filesystems common on macOS. A Unicode
normalization pass alone would not cover arbitrary non-UTF-8 Linux names or
every host filesystem's case rules, so non-ASCII archive paths fail closed.
Existing Harmony workload images use ASCII paths.
Archive member paths and hardlink targets are limited to 768 bytes, and
staging roots to 254 bytes in both their spelled and resolved forms. The
combined path then fits macOS's
1023-byte filesystem-call limit, including the separator, as well as Linux's.
GNU and macOS tar can both extract a deeply nested longer name, but later
rootfs traversal by absolute path fails only on macOS. The resolved staging
root length includes expansions such as macOS's `/var` to `/private/var`
without overlooking long path spellings that resolve to shorter directories.
Whiteout targets must match the spelling of the lower name they remove; after
an exact whiteout, a new entry may reuse that path with different case. Whiteout
lookups never follow a staged symlink parent. OCI control-file, mount, and
account-file lookups match each existing path component exactly rather than
using host case-folding; account paths cannot use symlinks. OCI layouts may
reuse a blob through a relative symlink whose target resolves, component by
component, to exact names inside that layout.
Local layout and archive paths also require exact spelling at every component;
symlink targets are checked component by component, including dangling links.
Relative targets are resolved from the link's parent, so readable ancestors
above that directory are not required. Explicit case-aliased host paths and
exact-name symlinks with invalid targets are rejected before staging.
An unqualified image name remains a container-tool reference unless its local
path has an exact match; an exact-name symlink with an invalid target is
rejected instead of falling through to the container tool. Absolute and
dot-relative paths explicitly select a local input and reject aliases.
Hardlink targets must resolve to an earlier member of the same archive with
the same case-sensitive path after removing `.` and repeated separators.
Hardlink, symlink, and directory members cannot carry
data in either their tar header or a pax size override. This also excludes
archives that only extract because a host resolves a hardlink target
case-insensitively or interprets a non-file payload differently.
An effective member name or hardlink target beginning with `\`, or an ASCII
letter followed by `:`, is rejected: macOS tar strips leading backslashes and
drive-like prefixes, while GNU tar retains them. The check uses the name or
target after pax and GNU overrides, so a portable override of an ambiguous
base header remains valid. A name beginning with `./\` remains portable.
An effective non-directory member name with a named path component followed
by terminal `/.` is rejected, including repeated slashes or dot components:
macOS tar can extract it as a file or link while GNU tar treats it as a
directory and errors. Directory members keep this spelling, and a pax or GNU
override to a portable name remains valid.
GNU long-name and long-link records and local PAX headers must have a following
member, even when their payload is empty or contains only ignored metadata.
macOS tar rejects orphaned extension records as damaged while GNU tar ignores
them, so staging rejects the archive before extraction.
Symlink targets must have a nonempty ASCII base header target and a nonempty
ASCII effective target after any pax or GNU override, with no embedded NUL.
An empty base target can become a regular file on macOS but a symlink on Linux;
an empty pax override and non-ASCII target bytes can also change extraction
success or the link's bytes between those hosts. Effective symlink targets are
limited to 1023 bytes: macOS rejects longer targets while Linux can create
them. Hardlinks likewise require a nonempty ASCII base header target even when
a pax or GNU override supplies a valid earlier target; an empty base can
extract as an empty regular file on macOS and a hardlink on Linux.
A hardlink cannot point to its own name, even when an earlier member created
that name: macOS tar rejects the repeated self-link while GNU tar accepts it.
A tar header whose magic starts with `ustar` but is neither POSIX ustar nor the
exact GNU header cannot carry a nonempty prefix field: macOS tar applies the
prefix while GNU tar ignores it and extracts a different name. Exact GNU
headers may use the early bytes for timestamps and remain accepted, but their
sparse-map, extension, and real-size fields must be empty: macOS tar can apply
them even to ordinary files while GNU tar ignores them.
Tar header timestamps must be valid nonnegative seconds representable as
signed 64-bit nanoseconds: macOS and GNU tar disagree on malformed, negative,
or larger header timestamps. PAX `mtime`, `atime`, and `ctime` records require
signed decimal values within that range, with an optional fractional part;
negative values cannot have a nonzero fraction. macOS tar interprets negative
fractions differently from GNU tar and clamps timestamps beyond the signed
nanosecond range, changing staged file metadata. The rootfs CPIO writer sets
every guest file timestamp to zero, but malformed PAX times can still make
GNU tar fail where macOS tar succeeds.
PAX size and owner fields require unsigned decimal digits. A leading plus
sign can make macOS extract a different payload size while GNU tar reports an
invalid extended header.
PAX record lengths also require decimal digits and each record must end in a
newline. PAX keys cannot be empty or contain whitespace or NUL: GNU tar skips
extra spaces and tabs after the record length while macOS tar does not, so an
otherwise ignored ` path` key can rename a member only on Linux. Malformed
framing fails admission before host extraction.
All tar numeric fields require octal digits with only space or NUL padding, or
a nonnegative base-256 value. An empty space-only field, a leading plus sign,
newline, or bytes after the NUL terminator can be interpreted differently by
host tar implementations.
Base UID and GID fields must also be valid and fit 32 bits when a PAX owner
record overrides them: GNU tar can report an error on an invalid base field
after macOS tar has accepted the same member.
The base member size must be below the signed 64-bit maximum even when a PAX
size overrides it: macOS tar reports overflow at that exact value while GNU
tar extracts the PAX-sized payload.
Device-major and device-minor header fields must have portable numeric syntax
even on ordinary files. macOS tar rejects malformed fields that GNU tar ignores;
fields containing only spaces and NULs, and base-256 encodings with a `0x80`
or `0xff` lead byte, remain admitted on both hosts.
Compressed archives must also finish decompression successfully after their
tar terminator; a bad gzip checksum can be ignored by macOS tar while GNU tar
reports failure despite both tools writing the member bytes. Gzip admission
uses one Rust decoder on every host and rejects trailing padding bytes: Apple
gzip rejects that padding while GNU gzip accepts it.
Other layer decompressors and tar run with a cleared child
environment, retaining only `PATH` for the tools and setting `LC_ALL=C`.
Inherited `XZ_DEFAULTS` can otherwise reject a valid archive before admission.
GNU tar honors inherited `TAR_OPTIONS` such as
`--strip-components=1`, while macOS tar honors `TAR_READER_OPTIONS` such as a
header charset conversion. Either could otherwise change an extracted path
after admission.
An entry whose name ends in `/` must carry the directory typeflag; some host
tar tools otherwise treat a regular file as a directory and parse its payload
as additional members that admission never saw. Empty effective member names,
including empty pax or GNU name overrides, are also rejected.
Members cannot combine GNU and pax overrides for a name or link target because
macOS and GNU tar choose different values when both are present.
A member also cannot have multiple pax headers, which those tar tools handle
differently.
An archive cannot change the member type at the same path: a symlink followed
by a regular file produces different final entries under macOS and GNU tar.
Repeated symlinks at one path are also rejected because macOS keeps the last
target while GNU tar can keep the first. Repeated ordinary files remain allowed.
Filesystem members are limited to ordinary files, directories, symlinks, and
hardlinks. Other tar typeflags are rejected before extraction because host tar
tools disagree on unknown members and special-file creation.
The hardlink inherits that earlier entry's owner and mode because both paths
name the same inode when the layer is extracted.

PAX `SCHILY.fflags` records are rejected before extraction: macOS tar can apply
immutable file flags that prevent staging, while GNU tar ignores the record.
PAX `SCHILY.acl.*` records are rejected for the same reason: macOS can enforce
an ACL that blocks the layer merge, while GNU tar ignores it without `--acls`.
PAX `SCHILY.realsize` is rejected as an unsupported sparse-file size record:
macOS tar expands the extracted file to that size while GNU tar ignores it.
PAX `SUN.holesdata` is also rejected: macOS tar applies its sparse map to file
contents while GNU tar ignores it.
Layer extraction disables extended-attribute restoration. macOS tar otherwise
tries to restore protected PAX xattrs and can fail where GNU tar succeeds;
extended attributes are not represented in the guest CPIO image.
Archive members with a `._`-prefixed path component are also rejected before
extraction. macOS tar can consume an AppleDouble member as metadata for a later
file, while GNU tar preserves it as a separate rootfs file.
Set `COPYFILE_DISABLE=1` when creating a layer with macOS tar so host metadata
does not introduce AppleDouble members.

A layer that ships a child without an entry for its parent directory preserves
an existing lower-layer directory's mode and ownership. A newly created
implicit parent has mode 0755 and root ownership, regardless of the host
process's umask. An explicit directory entry applies its header's mode and
ownership over the lower layer's. An opaque whiteout removes lower-layer
children while retaining the directory's metadata. Whiteout removal temporarily
grants owner read, write, and search access only to affected lower directories,
then restores their modes. Read-only lower directories therefore behave the
same under ordinary and privileged host users.
Preparation temporarily grants the staging user read access to files and
read/search access to directories while validating mount paths, resolving
credentials, and packaging the guest image, then restores their modes. The
guest CPIO keeps the archive's recorded modes, even for files the staging user
otherwise could not read. Standalone credential resolution uses the same
temporary access rule. Staged-image consumers that read files before bundle
preparation use `image::with_staged_read_access` for the same restore boundary.

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
discarded by the guest launcher. Preparation forces `LD_BIND_NOW=1` in both
supervisor and workload environments, replacing image-supplied values, so
admitted dynamic binding resolves symbols eagerly. This does not replace
executable/dependency admission or certify child programs that construct a new
environment; those remain part of the controlled-image qualification.

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
for ready, hook completion, pause, kill, and restart. It also restores a
portable snapshot and requires identical continuation evidence:

```sh
HARMONY_PLATFORM_KERNEL=/path/to/kernel \
HARMONY_PLATFORM_INITRAMFS=/path/to/initramfs-oci.cpio.gz \
HARMONY_PLATFORM_FIXTURE=/path/to/fixture-layout \
cargo test --locked --release -p oci-support --test process_platform -- --ignored --nocapture
```

## Host hashing

On native ARM64 Linux and macOS, SHA-256 uses runtime-detected CPU acceleration,
including when this component is built independently. Other targets and Miri
retain the existing backend selection. Hash inputs and outputs are unchanged.
The [host SHA qualification](../../scripts/qualification/README.md) checks independent builds and compares real consumers with software hashing.
