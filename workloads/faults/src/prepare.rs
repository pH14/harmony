// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, path::Path};

use guest_image::Writer;
use oci_support::image::Ownership;
use sha2::{Digest, Sha256};

use crate::bundle::FaultVocabulary;

pub const BUNDLE_PATH: &str = "etc/harmony/bundle";

pub struct Prepared {
    pub vocabulary: FaultVocabulary,
    pub bundle: String,
    pub initramfs: Vec<u8>,
}

pub fn prepare_oci(image: &str, base: &[u8], agent: &[u8]) -> Result<Prepared, Box<dyn Error>> {
    let staging = tempfile::tempdir()?;
    let staged = oci_support::image::stage(image, staging.path())?;
    prepare_rootfs(&staged.rootfs, &staged.owners, base, agent)
}

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
    let vocabulary =
        FaultVocabulary::parse(&bundle)?.with_instrumented_events(has_instrumented_events(&root));
    let mut image = base.to_vec();
    image.extend(oci_support::bundle::build_rootfs_segment(&root, owners)?);
    let mut overlay = Writer::new();
    for directory in ["harmony-oci/rootfs/opt", "harmony-oci/rootfs/opt/harmony"] {
        overlay.dir(directory, 0o755);
    }
    overlay.file("harmony-oci/rootfs/opt/harmony/fault-agent", 0o755, agent);
    overlay.file("init", 0o755, INIT);
    let control = overlay.finish();
    let padding = (4 - image.len() % 4) % 4;
    image.resize(image.len() + padding, 0);
    image.extend(control);
    Ok(Prepared {
        vocabulary,
        bundle,
        initramfs: image,
    })
}

fn rooted_file(root: &Path, relative: &str) -> Option<std::path::PathBuf> {
    let path = root.join(relative).canonicalize().ok()?;
    (path.starts_with(root) && path.is_file()).then_some(path)
}

fn has_instrumented_events(root: &Path) -> bool {
    if !rooted_file(root, "usr/lib/libvoidstar.so").is_some()
        || !valid_instrumented_event_attestation(root)
    {
        return false;
    }
    let Ok(symbols) = root.join("symbols").canonicalize() else {
        return false;
    };
    if !symbols.starts_with(root) {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(symbols) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(".sym.tsv"))
            && entry
                .path()
                .canonicalize()
                .is_ok_and(|path| path.starts_with(root))
            && entry
                .metadata()
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
    })
}

fn valid_instrumented_event_attestation(root: &Path) -> bool {
    let Some(attestation) = rooted_file(root, "symbols/harmony-instrumented-events") else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(attestation) else {
        return false;
    };
    text.lines().any(|line| {
        let Some((expected, image_path)) = line.split_once(char::is_whitespace) else {
            return false;
        };
        if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return false;
        }
        let image_path = image_path.trim().trim_start_matches('/');
        let Ok(executable) = root.join(image_path).canonicalize() else {
            return false;
        };
        if !executable.starts_with(root) {
            return false;
        }
        std::fs::read(executable).is_ok_and(|bytes| {
            format!("{:x}", Sha256::digest(bytes)) == expected.to_ascii_lowercase()
        })
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
    fn preparation_derives_instrumented_events_from_runtime_and_symbols() {
        let root = tempfile::tempdir().unwrap();
        image(root.path());
        std::fs::create_dir_all(root.path().join("usr/lib")).unwrap();
        std::fs::create_dir_all(root.path().join("symbols")).unwrap();
        std::fs::create_dir_all(root.path().join("opt/etcd")).unwrap();
        std::fs::write(root.path().join("usr/lib/libvoidstar.so"), b"runtime").unwrap();
        std::fs::write(root.path().join("symbols/etcd.sym.tsv"), b"1\tfile.go:1\n").unwrap();
        std::fs::write(root.path().join("opt/etcd/etcd"), b"instrumented node").unwrap();
        std::fs::write(
            root.path().join("symbols/harmony-instrumented-events"),
            b"f05c4a6fcf5bba49af1a80cf4015c096f55a4869c7df89cb85fa8737e993c995  /opt/etcd/etcd\n",
        )
        .unwrap();
        let prepared =
            prepare_rootfs(root.path(), &Ownership::default(), b"base", b"agent").unwrap();
        assert!(prepared.vocabulary.instrumented_events());

        std::fs::remove_file(root.path().join("symbols/etcd.sym.tsv")).unwrap();
        let no_symbols =
            prepare_rootfs(root.path(), &Ownership::default(), b"base", b"agent").unwrap();
        assert!(!no_symbols.vocabulary.instrumented_events());

        std::fs::write(root.path().join("symbols/etcd.sym.tsv"), b"1\tfile.go:1\n").unwrap();
        std::fs::remove_file(root.path().join("symbols/harmony-instrumented-events")).unwrap();
        let no_attestation =
            prepare_rootfs(root.path(), &Ownership::default(), b"base", b"agent").unwrap();
        assert!(!no_attestation.vocabulary.instrumented_events());

        std::fs::write(
            root.path().join("symbols/harmony-instrumented-events"),
            b"0000000000000000000000000000000000000000000000000000000000000000  /opt/etcd/etcd\n",
        )
        .unwrap();
        let wrong_hash =
            prepare_rootfs(root.path(), &Ownership::default(), b"base", b"agent").unwrap();
        assert!(!wrong_hash.vocabulary.instrumented_events());
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
