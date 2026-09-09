// SPDX-License-Identifier: AGPL-3.0-or-later
//! Prepare an OCI workload and the in-guest fault agent for deterministic
//! execution.
//!
//! The image contract is one file: `/etc/harmony/bundle`, the agent's own
//! bundle format. It names the workload's nodes, its hooks, its readiness
//! command and its one-time setup command, so the host learns the action
//! alphabet from the same text the guest agent obeys.

use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use guest_image::Writer;
use oci_support::image::Ownership;

use crate::bundle::FaultVocabulary;

/// Path the bundle occupies inside the workload image, and inside the guest.
pub const BUNDLE_PATH: &str = "etc/harmony/bundle";

/// A staged workload image and the action alphabet it declares.
pub struct Prepared {
    /// Nodes, hooks and places the bundle declares.
    pub vocabulary: FaultVocabulary,
    /// The bundle text, kept so a report can pin the exact contract.
    pub bundle: String,
    /// The assembled guest initramfs.
    pub initramfs: Vec<u8>,
}

/// Stage an OCI image without executing its commands on the host.
///
/// # Errors
///
/// Returns an error when the image cannot be staged, declares no bundle, or
/// declares one this package cannot act on.
pub fn prepare_oci(image: &str, base: &[u8], agent: &[u8]) -> Result<Prepared, Box<dyn Error>> {
    let staging = tempfile::tempdir()?;
    let staged = oci_support::image::stage(image, staging.path())?;
    prepare_rootfs(&staged.rootfs, &staged.owners, base, agent)
}

/// Assemble the guest initramfs from a staged rootfs. `owners` is the owner
/// each rootfs entry gets in the guest: a node that drops privileges to the
/// image's service account can only open what that account owns.
fn prepare_rootfs(
    rootfs: &Path,
    owners: &Ownership,
    base: &[u8],
    agent: &[u8],
) -> Result<Prepared, Box<dyn Error>> {
    if base.is_empty() || agent.is_empty() {
        return Err("fault search requires a guest base image and a static fault agent".into());
    }
    let root = rootfs.canonicalize()?;
    let document = root.join(BUNDLE_PATH).canonicalize()?;
    if !document.starts_with(&root) {
        return Err("the bundle must reside inside the staged image".into());
    }
    let bundle = String::from_utf8(std::fs::read(document)?)?;
    let vocabulary = FaultVocabulary::parse(&bundle)?.with_places(resolve_places(&root))?;
    let mut image = base.to_vec();
    image.extend(oci_support::bundle::build_rootfs_segment(&root, owners)?);
    let mut overlay = Writer::new();
    for directory in ["harmony-oci/rootfs/opt", "harmony-oci/rootfs/opt/harmony"] {
        overlay.dir(directory, 0o755);
    }
    overlay.file("harmony-oci/rootfs/opt/harmony/fault-agent", 0o755, agent);
    overlay.file("init", 0o755, INIT);
    let control = overlay.finish();
    // Linux accepts the next raw cpio header only at a four-byte boundary
    // after decompressing the preceding initramfs member.
    let padding = (4 - image.len() % 4) % 4;
    image.resize(image.len() + padding, 0);
    image.extend(control);
    Ok(Prepared {
        vocabulary,
        bundle,
        initramfs: image,
    })
}

/// Maximum number of automatically discovered execution places.
///
/// The place list is an acceleration surface, not a correctness condition:
/// the ordinary action alphabet remains available when a binary is not an ELF
/// image or when a host has no binary scanner. Keeping it bounded prevents a
/// large image full of helper binaries from diluting the searchable alphabet.
const MAX_AUTO_PLACES: usize = 128;

/// Resolve generic syscall boundaries from the workload's executable image.
///
/// Stock release binaries are often stripped, so a symbol-table-only resolver
/// would silently remove the most useful Park action. The cooperative kernel
/// can stop a process immediately before a syscall instruction instead. Those
/// instruction addresses are stable for the pinned binary, independent of the
/// host CPU and of workload-specific timing. We scan the node payload first;
/// helper binaries are considered only when it yields no places.
fn resolve_places(root: &Path) -> Vec<u64> {
    let mut executables = Vec::new();
    collect_executables(root, &mut executables);
    executables.sort_by_key(|path| (!path.starts_with(root.join("opt")), path.clone()));
    let mut places = Vec::new();
    for path in executables {
        places.extend(scan_syscall_instructions(&path));
        if places.len() >= MAX_AUTO_PLACES {
            break;
        }
    }
    places.sort_unstable();
    places.dedup();
    places.truncate(MAX_AUTO_PLACES);
    places
}

fn collect_executables(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_executables(&path, out);
        } else if file_type.is_file() && is_executable(&path) {
            out.push(path);
        }
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

/// Scan executable PT_LOAD segments for x86-64 `syscall` and arm64 `svc #0`.
fn scan_syscall_instructions(path: &Path) -> Vec<u64> {
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    if bytes.len() < 64 || bytes.get(0..4) != Some(&b"\x7fELF"[..]) || bytes[5] != 1 {
        return Vec::new();
    }
    let class = bytes[4];
    let Some(phoff) = read_u64(&bytes, 32) else {
        return Vec::new();
    };
    let Some(phentsize) = read_u16(&bytes, 54).map(usize::from) else {
        return Vec::new();
    };
    let Some(phnum) = read_u16(&bytes, 56).map(usize::from) else {
        return Vec::new();
    };
    if phentsize < 56 || phoff > bytes.len() as u64 {
        return Vec::new();
    }
    let mut places = Vec::new();
    for index in 0..phnum {
        let Some(header) = usize::try_from(phoff)
            .ok()
            .and_then(|start| start.checked_add(index.checked_mul(phentsize)?))
        else {
            break;
        };
        let Some(end) = header.checked_add(phentsize) else {
            break;
        };
        if end > bytes.len() {
            break;
        }
        let Some(kind) = read_u32_at(&bytes, header) else {
            continue;
        };
        let Some(flags) = read_u32_at(&bytes, header + 4) else {
            continue;
        };
        if kind != 1 || flags & 1 == 0 {
            continue;
        }
        let Some(offset) = read_u64_at(&bytes, header + 8) else {
            continue;
        };
        let Some(address) = read_u64_at(&bytes, header + 16) else {
            continue;
        };
        let Some(size) = read_u64_at(&bytes, header + 32) else {
            continue;
        };
        let Some(start) = usize::try_from(offset).ok() else {
            continue;
        };
        let Some(length) = usize::try_from(size).ok() else {
            continue;
        };
        let Some(stop) = start.checked_add(length).map(|end| end.min(bytes.len())) else {
            continue;
        };
        if stop <= start {
            continue;
        }
        let pattern: &[u8] = if class == 2 {
            b"\x0f\x05"
        } else {
            b"\x01\x00\x00\xd4"
        };
        for position in start..stop.saturating_sub(pattern.len()).saturating_add(1) {
            if bytes[position..position + pattern.len()] == *pattern {
                let Some(delta) = u64::try_from(position - start).ok() else {
                    continue;
                };
                places.push(address.saturating_add(delta));
            }
        }
    }
    places
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    bytes
        .get(offset..offset + 2)
        .map(|slice| u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    bytes
        .get(offset..offset + 4)
        .map(|slice| u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    bytes.get(offset..offset + 8).map(|slice| {
        u64::from_le_bytes([
            slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
        ])
    })
}

fn read_u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
    read_u64(bytes, offset)
}

const INIT: &[u8] = br##"#!/bin/sh
set -eu
# Kernel init environments need an explicit search path for guest tools.
export PATH=/sbin:/usr/sbin:/bin:/usr/bin
BB=/bin/busybox
stage=bootstrap
# Keep failures before the SDK transport opens visible on the guest console.
# The host can then distinguish an init/mount/chroot failure from a guest that
# reached the agent but stopped before publishing setup_complete.
trap 'rc=$?; echo "FAULT_INIT_EXIT stage=$stage rc=$rc" >&2' 0
echo "FAULT_INIT_STAGE=$stage" >&2
$BB mkdir -p /proc /sys /dev /run /tmp
# Kata's base initramfs has DEVTMPFS_MOUNT enabled, so /dev may already be
# mounted before this package-owned init runs.  Check each target first so a
# pre-mounted filesystem is accepted while a real mount failure still aborts
# startup under `set -e`.
stage=mount-proc
echo "FAULT_INIT_STAGE=$stage" >&2
if ! $BB grep -q ' /proc proc' /proc/mounts 2>/dev/null; then
    $BB mount -t proc proc /proc
fi
stage=mount-sys
echo "FAULT_INIT_STAGE=$stage" >&2
if ! $BB grep -q ' /sys sysfs' /proc/mounts 2>/dev/null; then
    $BB mount -t sysfs sysfs /sys
fi
stage=mount-dev
echo "FAULT_INIT_STAGE=$stage" >&2
if ! $BB grep -q ' /dev devtmpfs' /proc/mounts 2>/dev/null; then
    $BB mount -t devtmpfs dev /dev
fi
# The fault guest has a private network namespace with no external interface.
# Bring up its loopback device so local-only services (the normal distributed
# workload shape) work without a workload-specific setup knob.
$BB ip link set lo up
ROOT=/harmony-oci/rootfs
stage=prepare-rootfs
echo "FAULT_INIT_STAGE=$stage" >&2
$BB mkdir -p "$ROOT/run" "$ROOT/tmp" "$ROOT/run/fault-agent"
$BB chmod 1777 "$ROOT/tmp"
stage=bind-rootfs
echo "FAULT_INIT_STAGE=$stage" >&2
# iproute2's `ip netns exec` creates a mount namespace and makes `/` a
# recursive slave.  A plain chroot root is not a mountpoint, so that operation
# otherwise fails with EINVAL before the workload can configure its namespaces.
$BB mount --bind "$ROOT" "$ROOT"
stage=bind-pseudo-filesystems
echo "FAULT_INIT_STAGE=$stage" >&2
# The OCI rootfs is unpacked into the initramfs ramfs.  Keep the standard
# writable container paths on tmpfs so workloads can use their normal
# filesystem layout without depending on the size of the boot archive.  This
# also makes /tmp and /run behave the same way on every host and architecture.
$BB mount -t tmpfs tmpfs "$ROOT/tmp"
$BB mount -t tmpfs tmpfs "$ROOT/run"
$BB mkdir -p "$ROOT/run/fault-agent"
$BB chmod 1777 "$ROOT/tmp"
for directory in dev proc sys; do
    $BB mkdir -p "$ROOT/$directory"
    $BB mount --bind "/$directory" "$ROOT/$directory"
done
stage=chroot-agent
echo "FAULT_INIT_STAGE=$stage" >&2
exec $BB chroot "$ROOT" /opt/harmony/fault-agent \
    --bundle /etc/harmony/bundle --hook-dir /run/fault-agent
"##;

#[cfg(test)]
mod tests {
    use super::*;

    const BUNDLE: &str = "\
setup /bin/sh -c \"mkdir -p /tmp/etcd\"
node etcd /usr/bin/etcd --data-dir /tmp/etcd
hook 1 /hooks/compact
hook 2 /hooks/defrag
ready /usr/bin/etcdctl endpoint health
";

    fn image(root: &Path) {
        std::fs::create_dir_all(root.join("etc/harmony")).unwrap();
        std::fs::write(root.join(BUNDLE_PATH), BUNDLE).unwrap();
    }

    #[test]
    fn stripped_elf_syscall_places_are_resolved_from_executable_bytes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("opt/node");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut elf = vec![0_u8; 0x110];
        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2; // ELFCLASS64
        elf[5] = 1; // ELFDATA2LSB
        elf[32..40].copy_from_slice(&64_u64.to_le_bytes());
        elf[54..56].copy_from_slice(&56_u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1_u16.to_le_bytes());
        elf[64..68].copy_from_slice(&1_u32.to_le_bytes()); // PT_LOAD
        elf[68..72].copy_from_slice(&5_u32.to_le_bytes()); // PF_R | PF_X
        elf[72..80].copy_from_slice(&0x100_u64.to_le_bytes());
        elf[80..88].copy_from_slice(&0x400000_u64.to_le_bytes());
        elf[96..104].copy_from_slice(&0x10_u64.to_le_bytes());
        elf[0x101..0x103].copy_from_slice(b"\x0f\x05");
        std::fs::write(&path, elf).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(resolve_places(root.path()), [0x400001]);
    }

    #[test]
    fn preparation_pins_the_bundle_alphabet_and_the_agent_bytes() {
        let root = tempfile::tempdir().unwrap();
        image(root.path());
        let owners = Ownership::default();
        let first = prepare_rootfs(root.path(), &owners, b"base", b"agent-v1").unwrap();
        let repeated = prepare_rootfs(root.path(), &owners, b"base", b"agent-v1").unwrap();
        assert_eq!(first.vocabulary.nodes(), 1);
        assert_eq!(first.vocabulary.hooks(), [1, 2]);
        assert_eq!(first.bundle, BUNDLE);
        assert_eq!(first.initramfs, repeated.initramfs);
        assert_ne!(
            first.initramfs,
            prepare_rootfs(root.path(), &owners, b"base", b"agent-v2")
                .unwrap()
                .initramfs
        );
    }

    #[test]
    fn preparation_rejects_a_bundle_symlink_outside_the_image() {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::NamedTempFile::new().unwrap();
        std::fs::create_dir_all(root.path().join("etc/harmony")).unwrap();
        std::os::unix::fs::symlink(external.path(), root.path().join(BUNDLE_PATH)).unwrap();
        let error = prepare_rootfs(root.path(), &Ownership::default(), b"base", b"agent")
            .err()
            .unwrap();
        assert!(error.to_string().contains("inside the staged image"));
    }

    #[test]
    fn package_init_accepts_kernel_mounted_devtmpfs_without_hiding_mount_errors() {
        let contains = |needle: &[u8]| {
            INIT.windows(needle.len())
                .position(|window| window == needle)
        };
        let check = contains(b"if ! $BB grep -q ' /dev devtmpfs' /proc/mounts")
            .expect("devtmpfs mount check");
        let mount = contains(b"$BB mount -t devtmpfs dev /dev").expect("devtmpfs mount");
        let loopback = contains(b"$BB ip link set lo up").expect("loopback setup");
        assert!(check < mount, "the mount is guarded by /proc/mounts");
        assert!(contains(b"|| true").is_none());
        assert!(mount < loopback, "loopback follows /dev setup");
        assert!(contains(b"stage=mount-proc").is_some());
        assert!(contains(b"$BB mount --bind \"$ROOT\" \"$ROOT\"").is_some());
        let tmpfs_tmp =
            contains(b"$BB mount -t tmpfs tmpfs \"$ROOT/tmp\"").expect("workload /tmp tmpfs mount");
        let tmpfs_run =
            contains(b"$BB mount -t tmpfs tmpfs \"$ROOT/run\"").expect("workload /run tmpfs mount");
        let bind_rootfs = contains(b"stage=bind-rootfs").expect("bind-rootfs stage");
        let bind_pseudo =
            contains(b"stage=bind-pseudo-filesystems").expect("bind-pseudo-filesystems stage");
        let chroot = contains(b"stage=chroot-agent").expect("chroot stage");
        assert!(
            bind_rootfs < bind_pseudo,
            "root bind must precede child mounts"
        );
        assert!(bind_rootfs < tmpfs_tmp, "root bind must precede /tmp mount");
        assert!(tmpfs_tmp < tmpfs_run, "/tmp mount must precede /run mount");
        assert!(tmpfs_run < chroot, "writable mounts must precede chroot");
        assert!(bind_pseudo < chroot, "child mounts must precede chroot");
        assert!(contains(b"FAULT_INIT_EXIT stage=$stage rc=$rc").is_some());
        assert!(
            contains(b"/opt/harmony/fault-agent \\\n    --bundle /etc/harmony/bundle --hook-dir /run/fault-agent")
                .is_some()
        );
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("init");
        std::fs::write(&script, INIT).unwrap();
        assert!(
            std::process::Command::new("sh")
                .arg("-n")
                .arg(script)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn assembled_initramfs_aligns_raw_control_after_oci_gzip_member() {
        let root = tempfile::tempdir().unwrap();
        image(root.path());
        let base_root = tempfile::tempdir().unwrap();
        std::fs::write(base_root.path().join("init"), b"GUEST_READY\n").unwrap();
        let owners = Ownership::default();
        let base = oci_support::bundle::build_rootfs_segment(base_root.path(), &owners).unwrap();
        let oci_segment = oci_support::bundle::build_rootfs_segment(root.path(), &owners).unwrap();
        let prepared = prepare_rootfs(root.path(), &owners, &base, b"fault-agent").unwrap();

        assert!(base.starts_with(&[0x1f, 0x8b]));
        assert!(oci_segment.starts_with(&[0x1f, 0x8b]));
        let raw_start = base.len() + oci_segment.len();
        let aligned_start = raw_start.next_multiple_of(4);
        assert_eq!(
            prepared.initramfs[raw_start..aligned_start],
            vec![0; aligned_start - raw_start]
        );
        let control = &prepared.initramfs[aligned_start..];
        assert!(control.starts_with(b"070701"));
        assert!(
            control
                .windows(b"FAULT_INIT_STAGE=$stage".len())
                .any(|window| window == b"FAULT_INIT_STAGE=$stage")
        );
        assert!(
            control
                .windows(b"fault-agent".len())
                .any(|window| window == b"fault-agent")
        );
    }

    /// One `newc` entry: its name, `(uid, gid)`, and the offset of the next.
    fn cpio_entry(bytes: &[u8], at: usize) -> (String, (u32, u32), usize) {
        let field = |i: usize| {
            let text = std::str::from_utf8(&bytes[at + 6 + 8 * i..at + 6 + 8 * (i + 1)]).unwrap();
            u32::from_str_radix(text, 16).unwrap()
        };
        assert_eq!(&bytes[at..at + 6], b"070701");
        let (uid, gid, filesize, namesize) = (field(2), field(3), field(6), field(11));
        let name_at = at + 110;
        let name = std::str::from_utf8(&bytes[name_at..name_at + namesize as usize - 1])
            .unwrap()
            .to_string();
        let data_at = (name_at + namesize as usize).next_multiple_of(4);
        let next = (data_at + filesize as usize).next_multiple_of(4);
        (name, (uid, gid), next)
    }

    /// A node that drops to the image's service account can only open data
    /// that account owns in the guest, so the owner the image recorded has to
    /// survive into the initramfs even though the staged tree on the host
    /// belongs to whoever ran the staging.
    #[test]
    fn assembled_initramfs_keeps_the_owner_the_image_gave_each_entry() {
        let root = tempfile::tempdir().unwrap();
        image(root.path());
        let data = Path::new("var/lib/postgresql/data");
        std::fs::create_dir_all(root.path().join(data)).unwrap();
        std::fs::write(
            root.path().join(data).join("postgresql.conf"),
            b"port = 5432\n",
        )
        .unwrap();
        let mut owners = Ownership::default();
        let postgres = guest_image::Owner { uid: 70, gid: 70 };
        owners.record(Path::new("var/lib/postgresql"), postgres);
        owners.record(data, postgres);
        owners.record(&data.join("postgresql.conf"), postgres);
        let base =
            oci_support::bundle::build_rootfs_segment(root.path(), &Ownership::default()).unwrap();
        let prepared = prepare_rootfs(root.path(), &owners, &base, b"fault-agent").unwrap();

        let oci_segment = oci_support::bundle::build_rootfs_segment(root.path(), &owners).unwrap();
        assert_eq!(
            &prepared.initramfs[base.len()..base.len() + oci_segment.len()],
            &oci_segment[..]
        );
        let mut gzip = std::process::Command::new("gzip")
            .arg("-dc")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(gzip.stdin.as_mut().unwrap(), &oci_segment).unwrap();
        drop(gzip.stdin.take());
        let archive = gzip.wait_with_output().unwrap().stdout;
        let mut seen = std::collections::BTreeMap::new();
        let mut at = 0;
        loop {
            let (name, owner, next) = cpio_entry(&archive, at);
            if name == "TRAILER!!!" {
                break;
            }
            seen.insert(name, owner);
            at = next;
        }
        assert_eq!(seen["harmony-oci/rootfs/var/lib/postgresql"], (70, 70));
        assert_eq!(seen["harmony-oci/rootfs/var/lib/postgresql/data"], (70, 70));
        assert_eq!(
            seen["harmony-oci/rootfs/var/lib/postgresql/data/postgresql.conf"],
            (70, 70)
        );
        assert_eq!(seen["harmony-oci/rootfs/var/lib"], (0, 0));
        assert_eq!(seen["harmony-oci/rootfs/etc/harmony/bundle"], (0, 0));
    }
}
