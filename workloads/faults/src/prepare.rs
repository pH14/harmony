// SPDX-License-Identifier: AGPL-3.0-or-later
//! Prepare an OCI workload and the in-guest fault agent for deterministic
//! execution.
//!
//! The image contract is one file: `/etc/harmony/bundle`, the agent's own
//! bundle format. It names the workload's nodes, its hooks, its readiness
//! command and its one-time setup command, so the host learns the action
//! alphabet from the same text the guest agent obeys.

use std::{error::Error, path::Path};

use guest_image::Writer;

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
    prepare_rootfs(&staged.rootfs, base, agent)
}

fn prepare_rootfs(rootfs: &Path, base: &[u8], agent: &[u8]) -> Result<Prepared, Box<dyn Error>> {
    if base.is_empty() || agent.is_empty() {
        return Err("fault search requires a guest base image and a static fault agent".into());
    }
    let root = rootfs.canonicalize()?;
    let document = root.join(BUNDLE_PATH).canonicalize()?;
    if !document.starts_with(&root) {
        return Err("the bundle must reside inside the staged image".into());
    }
    let bundle = String::from_utf8(std::fs::read(document)?)?;
    let vocabulary = FaultVocabulary::parse(&bundle)?;
    let mut image = base.to_vec();
    image.extend(oci_support::bundle::build_rootfs_segment(&root)?);
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
node 0 etcd-0 /usr/local/bin/etcd-node 0
ready /usr/local/bin/etcd-ready
setup /usr/local/bin/etcd-setup
hook 1 /usr/local/bin/etcd-compact
hook 2 /usr/local/bin/etcd-defrag
";

    fn image(root: &Path) {
        std::fs::create_dir_all(root.join("etc/harmony")).unwrap();
        std::fs::write(root.join(BUNDLE_PATH), BUNDLE).unwrap();
    }

    #[test]
    fn preparation_pins_the_bundle_alphabet_and_the_agent_bytes() {
        let root = tempfile::tempdir().unwrap();
        image(root.path());
        let first = prepare_rootfs(root.path(), b"base", b"agent-v1").unwrap();
        let repeated = prepare_rootfs(root.path(), b"base", b"agent-v1").unwrap();
        assert_eq!(first.vocabulary.nodes(), 1);
        assert_eq!(first.vocabulary.hooks(), [1, 2]);
        assert_eq!(first.bundle, BUNDLE);
        assert_eq!(first.initramfs, repeated.initramfs);
        assert_ne!(
            first.initramfs,
            prepare_rootfs(root.path(), b"base", b"agent-v2")
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
        let error = prepare_rootfs(root.path(), b"base", b"agent")
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
        assert!(check < mount, "the mount is guarded by /proc/mounts");
        assert!(contains(b"|| true").is_none());
        assert!(contains(b"stage=mount-proc").is_some());
        assert!(contains(b"$BB mount --bind \"$ROOT\" \"$ROOT\"").is_some());
        let bind_rootfs = contains(b"stage=bind-rootfs").expect("bind-rootfs stage");
        let bind_pseudo =
            contains(b"stage=bind-pseudo-filesystems").expect("bind-pseudo-filesystems stage");
        let chroot = contains(b"stage=chroot-agent").expect("chroot stage");
        assert!(
            bind_rootfs < bind_pseudo,
            "root bind must precede child mounts"
        );
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
        let base = oci_support::bundle::build_rootfs_segment(base_root.path()).unwrap();
        let oci_segment = oci_support::bundle::build_rootfs_segment(root.path()).unwrap();
        let prepared = prepare_rootfs(root.path(), &base, b"fault-agent").unwrap();

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
}
