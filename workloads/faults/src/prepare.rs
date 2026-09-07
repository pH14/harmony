// SPDX-License-Identifier: AGPL-3.0-or-later
//! Prepare an OCI workload and its guest supervisor for deterministic execution.
use crate::WorkloadSpec;
use guest_image::Writer;
use std::{error::Error, path::Path};

pub struct Prepared {
    pub spec: WorkloadSpec,
    pub initramfs: Vec<u8>,
}

/// Stage an OCI image without executing its commands on the host.
/// The image supplies `/harmony/workload.json`; the supervisor runs in its rootfs.
pub fn prepare_oci(image: &str, base: &[u8], agent: &[u8]) -> Result<Prepared, Box<dyn Error>> {
    let staging = tempfile::tempdir()?;
    let staged = oci_support::image::stage(image, staging.path())?;
    prepare_rootfs(&staged.rootfs, base, agent)
}

fn prepare_rootfs(rootfs: &Path, base: &[u8], agent: &[u8]) -> Result<Prepared, Box<dyn Error>> {
    if base.is_empty() || agent.is_empty() {
        return Err(
            "fault search requires a guest base image and static fault-guest executable".into(),
        );
    }
    let root = rootfs.canonicalize()?;
    let document = root.join("harmony/workload.json").canonicalize()?;
    if !document.starts_with(&root) {
        return Err("workload.json must reside inside the staged image".into());
    }
    let spec: WorkloadSpec = serde_json::from_slice(&std::fs::read(document)?)?;
    spec.validate()?;
    let mut image = base.to_vec();
    image.extend(oci_support::bundle::build_rootfs_segment(&root)?);
    let mut overlay = Writer::new();
    for directory in ["harmony-oci/rootfs/opt", "harmony-oci/rootfs/opt/harmony"] {
        overlay.dir(directory, 0o755);
    }
    overlay.file("harmony-oci/rootfs/opt/harmony/fault-guest", 0o755, agent);
    overlay.file("init", 0o755, INIT);
    let control = overlay.finish();
    // Linux accepts the next raw cpio header only at a four-byte boundary
    // after decompressing the preceding initramfs member.
    let padding = (4 - image.len() % 4) % 4;
    image.resize(image.len() + padding, 0);
    image.extend(control);
    Ok(Prepared {
        spec,
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
# reached the supervisor but stopped before publishing setup_complete.
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
$BB mkdir -p "$ROOT/run" "$ROOT/tmp"
$BB chmod 1777 "$ROOT/tmp"
stage=bind-rootfs
echo "FAULT_INIT_STAGE=$stage" >&2
# iproute2's `ip netns exec` creates a mount namespace and makes `/` a
# recursive slave.  A plain chroot root is not a mountpoint, so that operation
# otherwise fails with EINVAL before the fixture can configure its namespaces.
$BB mount --bind "$ROOT" "$ROOT"
stage=bind-pseudo-filesystems
echo "FAULT_INIT_STAGE=$stage" >&2
for directory in dev proc sys; do
    $BB mkdir -p "$ROOT/$directory"
    $BB mount --bind "/$directory" "$ROOT/$directory"
done
stage=chroot-supervisor
echo "FAULT_INIT_STAGE=$stage" >&2
exec $BB chroot "$ROOT" /opt/harmony/fault-guest
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CommandSpec, NetworkSpec, NodeSpec};

    #[test]
    fn preparation_pins_image_spec_and_supervisor_bytes() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("harmony")).unwrap();
        let spec = WorkloadSpec {
            version: crate::spec::WORKLOAD_SCHEMA_VERSION,
            setup: None,
            readiness: None,
            nodes: vec![
                NodeSpec::new(0, "a", "/a", "/"),
                NodeSpec::new(1, "b", "/b", "/"),
            ],
            network: vec![NetworkSpec {
                from: 0,
                to: 1,
                interface: "a-b".into(),
            }],
            check: CommandSpec::new("/check", "/"),
            pending: None,
            recovery: None,
        };
        std::fs::write(
            root.path().join("harmony/workload.json"),
            spec.encode_json().unwrap(),
        )
        .unwrap();
        let first = prepare_rootfs(root.path(), b"base", b"agent-v1").unwrap();
        let repeated = prepare_rootfs(root.path(), b"base", b"agent-v1").unwrap();
        assert_eq!(first.spec, spec);
        assert_eq!(first.initramfs, repeated.initramfs);
        assert_ne!(
            first.initramfs,
            prepare_rootfs(root.path(), b"base", b"agent-v2")
                .unwrap()
                .initramfs
        );
    }

    #[test]
    fn preparation_rejects_spec_symlink_outside_image() {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::NamedTempFile::new().unwrap();
        std::fs::create_dir(root.path().join("harmony")).unwrap();
        std::os::unix::fs::symlink(external.path(), root.path().join("harmony/workload.json"))
            .unwrap();
        let error = prepare_rootfs(root.path(), b"base", b"agent")
            .err()
            .unwrap();
        assert!(error.to_string().contains("inside the staged image"));
    }

    #[test]
    fn package_init_accepts_kernel_mounted_devtmpfs_without_hiding_mount_errors() {
        let check = INIT
            .windows(b"if ! $BB grep -q ' /dev devtmpfs' /proc/mounts".len())
            .position(|window| window == b"if ! $BB grep -q ' /dev devtmpfs' /proc/mounts")
            .expect("devtmpfs mount check");
        let mount = INIT
            .windows(b"$BB mount -t devtmpfs dev /dev".len())
            .position(|window| window == b"$BB mount -t devtmpfs dev /dev")
            .expect("devtmpfs mount");
        assert!(check < mount, "the mount is guarded by /proc/mounts");
        assert!(
            !INIT
                .windows(b"|| true".len())
                .any(|window| window == b"|| true")
        );
        assert!(
            INIT.windows(b"stage=mount-proc".len())
                .any(|window| { window == b"stage=mount-proc" })
        );
        assert!(
            INIT.windows(b"stage=bind-rootfs".len())
                .any(|window| { window == b"stage=bind-rootfs" })
        );
        assert!(
            INIT.windows(b"$BB mount --bind \"$ROOT\" \"$ROOT\"".len())
                .any(|window| { window == b"$BB mount --bind \"$ROOT\" \"$ROOT\"" })
        );
        let bind_rootfs = INIT
            .windows(b"stage=bind-rootfs".len())
            .position(|window| window == b"stage=bind-rootfs")
            .expect("bind-rootfs stage");
        let chroot = INIT
            .windows(b"stage=chroot-supervisor".len())
            .position(|window| window == b"stage=chroot-supervisor")
            .expect("chroot stage");
        let bind_pseudo = INIT
            .windows(b"stage=bind-pseudo-filesystems".len())
            .position(|window| window == b"stage=bind-pseudo-filesystems")
            .expect("bind-pseudo-filesystems stage");
        assert!(
            bind_rootfs < bind_pseudo,
            "root bind must precede child mounts"
        );
        assert!(bind_pseudo < chroot, "child mounts must precede chroot");
        assert!(
            INIT.windows(b"FAULT_INIT_EXIT stage=$stage rc=$rc".len())
                .any(|window| { window == b"FAULT_INIT_EXIT stage=$stage rc=$rc" })
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
        std::fs::create_dir(root.path().join("harmony")).unwrap();
        let spec = WorkloadSpec {
            version: crate::spec::WORKLOAD_SCHEMA_VERSION,
            setup: None,
            readiness: None,
            nodes: vec![
                NodeSpec::new(0, "a", "/a", "/"),
                NodeSpec::new(1, "b", "/b", "/"),
            ],
            network: vec![NetworkSpec {
                from: 0,
                to: 1,
                interface: "a-b".into(),
            }],
            check: CommandSpec::new("/check", "/"),
            pending: None,
            recovery: None,
        };
        std::fs::write(
            root.path().join("harmony/workload.json"),
            spec.encode_json().unwrap(),
        )
        .unwrap();

        let base_root = tempfile::tempdir().unwrap();
        std::fs::write(base_root.path().join("init"), b"GUEST_READY\n").unwrap();
        let base = oci_support::bundle::build_rootfs_segment(base_root.path()).unwrap();
        let oci_segment = oci_support::bundle::build_rootfs_segment(root.path()).unwrap();
        let prepared = prepare_rootfs(root.path(), &base, b"fault-agent").unwrap();

        assert!(base.starts_with(&[0x1f, 0x8b]));
        assert!(oci_segment.starts_with(&[0x1f, 0x8b]));
        let raw_start = base.len() + oci_segment.len();
        let aligned_start = (raw_start + 3) & !3;
        assert_eq!(
            prepared.initramfs[raw_start..aligned_start],
            vec![0; aligned_start - raw_start]
        );
        assert_eq!(aligned_start % 4, 0);
        let control = &prepared.initramfs[aligned_start..];
        assert!(control.starts_with(b"070701"));
        let package_marker = control
            .windows(b"FAULT_INIT_STAGE=$stage".len())
            .position(|window| window == b"FAULT_INIT_STAGE=$stage")
            .expect("package init member");
        assert!(package_marker > 0);
        assert!(
            control
                .windows(b"fault-agent".len())
                .any(|window| { window == b"fault-agent" })
        );
    }
}
