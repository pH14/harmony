// SPDX-License-Identifier: AGPL-3.0-or-later
//! Build the injected initramfs segment for a staged image: the container
//! bundle (`/harmony-oci/rootfs` + runc `config.json`) plus the init script
//! the kernel starts via `rdinit=/harmony-oci-init`.
//!
//! The stock guest initramfs supplies busybox (and `runc` where the image
//! variant carries it); this segment supplies everything workload-specific.
//! When `runc` is absent the init falls back to a chroot start, which keeps
//! the run deterministic — isolation fidelity, not determinism, is what the
//! fallback gives up.

use super::cpio::{CpioError, Writer};
use super::image::StagedImage;
use serde_json::json;
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error(transparent)]
    Cpio(#[from] CpioError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("gzip failed: {0}")]
    Gzip(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// The container's argv: the CLI override verbatim when given, else
/// entrypoint followed by cmd per the OCI image spec.
fn argv(image: &StagedImage, cmd_override: &[String]) -> Vec<String> {
    if !cmd_override.is_empty() {
        return cmd_override.to_vec();
    }
    let mut argv = image.config.entrypoint.clone();
    argv.extend(image.config.cmd.iter().cloned());
    if argv.is_empty() {
        argv.push("/bin/sh".to_string());
    }
    argv
}

fn env(image: &StagedImage) -> Vec<String> {
    let mut env = image.config.env.clone();
    if !env.iter().any(|e| e.starts_with("PATH=")) {
        env.push("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into());
    }
    env
}

fn cwd(image: &StagedImage) -> String {
    match image.config.working_dir.as_deref() {
        Some("") | None => "/".to_string(),
        Some(dir) => dir.to_string(),
    }
}

/// runc spec: single-vCPU guest, allow-all devices, `terminal = false` — the
/// same shape the proven runc-postgres guest bundle uses.
fn runc_spec(image: &StagedImage, cmd_override: &[String]) -> serde_json::Value {
    json!({
        "ociVersion": "1.0.2",
        "process": {
            "terminal": false,
            "user": { "uid": 0, "gid": 0 },
            "args": argv(image, cmd_override),
            "env": env(image),
            "cwd": cwd(image),
        },
        "root": { "path": "rootfs" },
        "hostname": "harmony",
        "mounts": [
            { "destination": "/proc", "type": "proc", "source": "proc" },
            { "destination": "/dev", "type": "tmpfs", "source": "tmpfs",
              "options": ["nosuid", "strictatime", "mode=755", "size=65536k"] },
            { "destination": "/dev/pts", "type": "devpts", "source": "devpts",
              "options": ["nosuid", "noexec", "newinstance", "ptmxmode=0666", "mode=0620"] },
            { "destination": "/dev/shm", "type": "tmpfs", "source": "shm",
              "options": ["nosuid", "noexec", "nodev", "mode=1777", "size=65536k"] },
            { "destination": "/sys", "type": "sysfs", "source": "sysfs",
              "options": ["nosuid", "noexec", "nodev", "ro"] },
            { "destination": "/tmp", "type": "tmpfs", "source": "tmpfs" },
        ],
        "linux": {
            "namespaces": [
                { "type": "pid" }, { "type": "ipc" },
                { "type": "uts" }, { "type": "mount" },
            ],
            "resources": { "devices": [ { "allow": true, "access": "rwm" } ] },
        },
    })
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The chroot-fallback start script, written inside the rootfs.
fn start_script(image: &StagedImage, cmd_override: &[String]) -> String {
    let mut script = String::from("#!/bin/sh\n");
    for var in env(image) {
        if let Some((key, value)) = var.split_once('=') {
            script.push_str(&format!("export {key}={}\n", shell_quote(value)));
        }
    }
    script.push_str(&format!("cd {} || exit 125\n", shell_quote(&cwd(image))));
    let args: Vec<String> = argv(image, cmd_override)
        .iter()
        .map(|a| shell_quote(a))
        .collect();
    script.push_str(&format!("exec {}\n", args.join(" ")));
    script
}

/// PID-1 for the run: mount the pseudo-filesystems, start the container via
/// runc (or chroot when the base initramfs has no runc), report its exit
/// status on the serial console, and power off. `panic=-1` + forced reboot in
/// the kernel cmdline turn any failure here into a terminal exit, never a
/// hang.
#[cfg(target_arch = "x86_64")]
const INIT: &str = r#"#!/bin/sh
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
mkdir -p /dev/pts /dev/shm /run /tmp
mount -t devpts devpts /dev/pts 2>/dev/null
mount -t tmpfs tmpfs /run 2>/dev/null
mount -t cgroup2 none /sys/fs/cgroup 2>/dev/null
echo HARMONY_OCI: start
cd /harmony-oci
runc run --bundle /harmony-oci harmony-c1
rc=$?
case $rc in
127)
    # No runc in this guest image: chroot start (same determinism, thinner
    # isolation). The minimal ash has no `test`/`command` builtins, so runc's
    # absence is detected by its 127 exit status.
    echo HARMONY_OCI: via chroot
    mount -t proc proc /harmony-oci/rootfs/proc 2>/dev/null
    mount -t devtmpfs devtmpfs /harmony-oci/rootfs/dev 2>/dev/null
    mkdir -p /harmony-oci/rootfs/dev/shm /harmony-oci/rootfs/dev/pts
    mount -t tmpfs tmpfs /harmony-oci/rootfs/dev/shm 2>/dev/null
    mount -t devpts devpts /harmony-oci/rootfs/dev/pts 2>/dev/null
    mount -t sysfs sysfs /harmony-oci/rootfs/sys 2>/dev/null
    mount -t tmpfs tmpfs /harmony-oci/rootfs/tmp 2>/dev/null
    mount -t tmpfs tmpfs /harmony-oci/rootfs/run 2>/dev/null
    chroot /harmony-oci/rootfs /bin/sh /.harmony-start.sh
    rc=$?
    ;;
*)
    echo HARMONY_OCI: via runc
    ;;
esac
echo HARMONY_OCI_EXIT rc=$rc
poweroff -f
reboot -f
echo o > /proc/sysrq-trigger
"#;

/// arm64: the harness DTB's pl011 node is frozen without the primecell/clock
/// properties the console driver needs to probe, so there is no /dev/console
/// tty. All run output goes through /bin/mmio-console (shipped in
/// initramfs-oci), which writes the PL011 data register directly via
/// /dev/mem — the transport the postgres guest image proved.
#[cfg(not(target_arch = "x86_64"))]
const INIT: &str = r#"#!/bin/sh
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
mkdir -p /dev/pts /dev/shm /run /tmp
mount -t devpts devpts /dev/pts 2>/dev/null
mount -t tmpfs tmpfs /run 2>/dev/null
mount -t cgroup2 none /sys/fs/cgroup 2>/dev/null
{
    echo HARMONY_OCI: start
    cd /harmony-oci
    runc run --bundle /harmony-oci harmony-c1
    rc=$?
    case $rc in
    127)
        # No runc in this guest image: chroot start (same determinism, thinner
        # isolation). The minimal ash has no `test`/`command` builtins, so
        # runc's absence is detected by its 127 exit status.
        echo HARMONY_OCI: via chroot
        mount -t proc proc /harmony-oci/rootfs/proc 2>/dev/null
        mount -t devtmpfs devtmpfs /harmony-oci/rootfs/dev 2>/dev/null
        mkdir -p /harmony-oci/rootfs/dev/shm /harmony-oci/rootfs/dev/pts
        mount -t tmpfs tmpfs /harmony-oci/rootfs/dev/shm 2>/dev/null
        mount -t devpts devpts /harmony-oci/rootfs/dev/pts 2>/dev/null
        mount -t sysfs sysfs /harmony-oci/rootfs/sys 2>/dev/null
        mount -t tmpfs tmpfs /harmony-oci/rootfs/tmp 2>/dev/null
        mount -t tmpfs tmpfs /harmony-oci/rootfs/run 2>/dev/null
        chroot /harmony-oci/rootfs /bin/sh /.harmony-start.sh
        rc=$?
        ;;
    *)
        echo HARMONY_OCI: via runc
        ;;
    esac
    echo HARMONY_OCI_EXIT rc=$rc
} 2>&1 | /bin/mmio-console
poweroff -f
reboot -f
"#;

/// Assemble the gzip-compressed initramfs segment for `image`.
pub fn build_segment(image: &StagedImage, cmd_override: &[String]) -> Result<Vec<u8>, BundleError> {
    let mut w = Writer::new();
    w.file("harmony-oci-init", 0o755, INIT.as_bytes());
    w.dir("harmony-oci", 0o755);
    w.file(
        "harmony-oci/config.json",
        0o644,
        &serde_json::to_vec_pretty(&runc_spec(image, cmd_override))?,
    );
    w.dir("harmony-oci/rootfs", 0o755);
    w.file(
        "harmony-oci/rootfs/.harmony-start.sh",
        0o755,
        start_script(image, cmd_override).as_bytes(),
    );
    w.tree(&image.rootfs, "harmony-oci/rootfs")?;
    gzip(&w.finish())
}

/// `gzip -n` omits name/mtime, keeping the segment bytes a pure function of
/// its contents.
fn gzip(data: &[u8]) -> Result<Vec<u8>, BundleError> {
    // Feed gzip from a file, not a stdin pipe: writing a multi-megabyte
    // segment into a pipe while gzip's stdout pipe is unread deadlocks both
    // processes at the kernel pipe buffer size.
    let mut input = tempfile::NamedTempFile::new()?;
    std::io::Write::write_all(&mut input, data)?;
    let out = Command::new("gzip")
        .args(["-n", "-9", "-c"])
        .arg(input.path())
        .output()?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(BundleError::Gzip(
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ))
    }
}
