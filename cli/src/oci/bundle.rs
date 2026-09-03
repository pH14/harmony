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
use super::image::RuntimeConfig;
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
fn argv(image: &RuntimeConfig, cmd_override: &[String]) -> Vec<String> {
    if !cmd_override.is_empty() {
        return cmd_override.to_vec();
    }
    let mut argv = image.entrypoint.clone();
    argv.extend(image.cmd.iter().cloned());
    if argv.is_empty() {
        argv.push("/bin/sh".to_string());
    }
    argv
}

fn env(image: &RuntimeConfig) -> Vec<String> {
    let mut env = image.env.clone();
    if !env.iter().any(|e| e.starts_with("PATH=")) {
        env.push("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into());
    }
    env
}

fn cwd(image: &RuntimeConfig) -> String {
    match image.working_dir.as_deref() {
        Some("") | None => "/".to_string(),
        Some(dir) => dir.to_string(),
    }
}

/// runc spec: single-vCPU guest, allow-all devices, `terminal = false` — the
/// same shape the proven runc-postgres guest bundle uses.
fn runc_spec(image: &RuntimeConfig, cmd_override: &[String]) -> serde_json::Value {
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
fn start_script(image: &RuntimeConfig, cmd_override: &[String]) -> String {
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

/// Mount the pseudo-filesystems both container start paths need.
const INIT_MOUNTS: &str = r#"export PATH=/usr/sbin:/usr/bin:/sbin:/bin
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
mkdir -p /dev/pts /dev/shm /run /tmp
mount -t devpts devpts /dev/pts 2>/dev/null
mount -t tmpfs tmpfs /run 2>/dev/null
mount -t cgroup2 none /sys/fs/cgroup 2>/dev/null
"#;

/// Start the container and report its exit status on the console.
///
/// The start path is chosen by probing runc, before the workload runs. The
/// probe's 127 means no runc in this guest image; the workload's own 127
/// means the workload could not find its command, and reading that as an
/// absent runc would start the workload a second time. The minimal ash has no
/// `test` or `command` builtin, so `case` on the probe's status is the
/// available form.
const INIT_START: &str = r#"echo HARMONY_OCI: start
runc --version >/dev/null 2>&1
case $? in
127)
    # No runc in this guest image: chroot start (same determinism, thinner
    # isolation).
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
    cd /harmony-oci
    runc run --bundle /harmony-oci harmony-c1
    rc=$?
    ;;
esac
echo HARMONY_OCI_EXIT rc=$rc
"#;

/// Power off. `panic=-1` + forced reboot in the x86 kernel cmdline turn any
/// failure before here into a terminal exit, never a hang; the sysrq write is
/// the last resort if both commands are unavailable.
const INIT_POWEROFF: &str = r#"poweroff -f
reboot -f
echo o > /proc/sysrq-trigger
"#;

/// Where the init's output goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Console {
    /// x86: the kernel's console device on ttyS0.
    Serial,
    /// arm64: the harness DTB's pl011 node is frozen without the
    /// primecell/clock properties the console driver needs to probe, so there
    /// is no /dev/console tty. Output goes through /bin/mmio-console (shipped
    /// in initramfs-oci), which writes the PL011 data register directly via
    /// /dev/mem — the transport the postgres guest image proved.
    Mmio,
}

/// PID-1 for the run: mount the pseudo-filesystems, start the container,
/// report its exit status on the console, and power off.
fn init_script(console: Console) -> String {
    let start = match console {
        Console::Serial => INIT_START.to_string(),
        Console::Mmio => format!("{{\n{INIT_START}}} 2>&1 | /bin/mmio-console\n"),
    };
    format!("#!/bin/sh\n{INIT_MOUNTS}{start}{INIT_POWEROFF}")
}

fn init() -> String {
    init_script(if cfg!(target_arch = "x86_64") {
        Console::Serial
    } else {
        Console::Mmio
    })
}

/// Assemble the gzip-compressed rootfs initramfs segment: the unpacked image
/// tree under `harmony-oci/rootfs`. A pure function of the tree, so it is
/// cacheable by image identity.
pub fn build_rootfs_segment(rootfs: &std::path::Path) -> Result<Vec<u8>, BundleError> {
    let mut w = Writer::new();
    w.dir("harmony-oci", 0o755);
    w.dir("harmony-oci/rootfs", 0o755);
    w.tree(rootfs, "harmony-oci/rootfs")?;
    gzip(&w.finish())
}

/// Assemble the gzip-compressed control initramfs segment: the injected init,
/// the runc spec, and the chroot start script. Later cpio entries override
/// earlier ones, so this segment can add files under the cached rootfs
/// segment's directories.
pub fn build_control_segment(
    config: &RuntimeConfig,
    cmd_override: &[String],
) -> Result<Vec<u8>, BundleError> {
    let mut w = Writer::new();
    w.file("harmony-oci-init", 0o755, init().as_bytes());
    w.dir("harmony-oci", 0o755);
    w.file(
        "harmony-oci/config.json",
        0o644,
        &serde_json::to_vec_pretty(&runc_spec(config, cmd_override))?,
    );
    w.file(
        "harmony-oci/rootfs/.harmony-start.sh",
        0o755,
        start_script(config, cmd_override).as_bytes(),
    );
    gzip(&w.finish())
}

/// `gzip -n` omits name/mtime, keeping the segment bytes a pure function of
/// its contents. Level 1: the segment is decompressed once by the kernel and
/// thrown away, and level 9 costs ~8x the wall time of the whole guest run
/// on a container-sized rootfs for ~8% smaller output.
fn gzip(data: &[u8]) -> Result<Vec<u8>, BundleError> {
    // Feed gzip from a file, not a stdin pipe: writing a multi-megabyte
    // segment into a pipe while gzip's stdout pipe is unread deadlocks both
    // processes at the kernel pipe buffer size.
    let mut input = tempfile::NamedTempFile::new()?;
    std::io::Write::write_all(&mut input, data)?;
    let out = Command::new("gzip")
        .args(["-n", "-1", "-c"])
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> RuntimeConfig {
        RuntimeConfig {
            entrypoint: vec!["docker-entrypoint.sh".into()],
            cmd: vec!["postgres".into()],
            env: vec!["PGDATA=/var/lib/postgresql/data".into()],
            working_dir: Some("/app".into()),
        }
    }

    #[test]
    fn argv_is_entrypoint_then_cmd_unless_overridden() {
        assert_eq!(
            argv(&config(), &[]),
            vec!["docker-entrypoint.sh".to_string(), "postgres".into()]
        );
        assert_eq!(
            argv(&config(), &["echo".to_string(), "hi".into()]),
            vec!["echo".to_string(), "hi".into()]
        );
        assert_eq!(
            argv(&RuntimeConfig::default(), &[]),
            vec!["/bin/sh".to_string()]
        );
    }

    #[test]
    fn env_appends_default_path_only_when_absent() {
        let with_path = RuntimeConfig {
            env: vec!["PATH=/custom".into()],
            ..RuntimeConfig::default()
        };
        assert_eq!(env(&with_path), vec!["PATH=/custom".to_string()]);
        let got = env(&config());
        assert_eq!(got[0], "PGDATA=/var/lib/postgresql/data");
        assert!(got[1].starts_with("PATH=/usr/local/sbin:"));
    }

    #[test]
    fn cwd_defaults_to_root() {
        assert_eq!(cwd(&RuntimeConfig::default()), "/");
        let empty = RuntimeConfig {
            working_dir: Some(String::new()),
            ..RuntimeConfig::default()
        };
        assert_eq!(cwd(&empty), "/");
        assert_eq!(cwd(&config()), "/app");
    }

    #[test]
    fn shell_quote_survives_embedded_quotes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn runc_spec_carries_process_facts() {
        let spec = runc_spec(&config(), &[]);
        assert_eq!(spec["process"]["cwd"], "/app");
        assert_eq!(spec["process"]["args"][0], "docker-entrypoint.sh");
        assert_eq!(spec["root"]["path"], "rootfs");
    }

    #[test]
    fn start_script_exports_env_and_execs_argv() {
        let script = start_script(&config(), &[]);
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains("export PGDATA='/var/lib/postgresql/data'\n"));
        assert!(script.contains("cd '/app' || exit 125\n"));
        assert!(script.contains("exec 'docker-entrypoint.sh' 'postgres'\n"));
    }

    /// The start path must come from a probe of runc, never from the
    /// workload's exit status: a container that legitimately exits 127 would
    /// otherwise be started a second time through the chroot fallback.
    #[test]
    fn init_selects_the_start_path_before_running_the_workload() {
        let probe = INIT_START.find("runc --version").expect("runc probe");
        let switch = INIT_START.find("case $? in").expect("probe status switch");
        let runc = INIT_START.find("runc run --bundle").expect("runc start");
        let chroot = INIT_START
            .find("chroot /harmony-oci/rootfs")
            .expect("chroot start");
        assert!(probe < switch, "the probe must precede the switch");
        assert!(switch < runc && switch < chroot, "both paths sit under it");
        // Each start path appears once, so the workload runs exactly once.
        assert_eq!(INIT_START.matches("runc run --bundle").count(), 1);
        assert_eq!(INIT_START.matches("chroot /harmony-oci/rootfs").count(), 1);
        // Nothing branches on the workload's own status.
        assert!(!INIT_START.contains("case $rc"));
        assert_eq!(INIT_START.matches("rc=$?").count(), 2);
        assert!(INIT_START.rfind("rc=$?").unwrap() < INIT_START.find("HARMONY_OCI_EXIT").unwrap());
    }

    /// Both console variants must parse as POSIX shell: the guest runs them
    /// as PID 1, where a syntax error is an unbootable run.
    #[test]
    fn init_variants_are_valid_shell() {
        for console in [Console::Serial, Console::Mmio] {
            let script = init_script(console);
            let mut file = tempfile::NamedTempFile::new().unwrap();
            std::io::Write::write_all(&mut file, script.as_bytes()).unwrap();
            let out = Command::new("/bin/sh")
                .arg("-n")
                .arg(file.path())
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{console:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(script.starts_with("#!/bin/sh\n"));
        }
        // Only the arm64 variant routes output through the mmio console.
        assert!(init_script(Console::Mmio).contains("| /bin/mmio-console"));
        assert!(!init_script(Console::Serial).contains("mmio-console"));
    }

    /// Both segment builders must produce real gzip members (the kernel
    /// decompresses concatenated members), stably.
    #[test]
    fn segments_are_gzip_members_and_reproducible() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello"), b"payload").unwrap();
        let rootfs = build_rootfs_segment(dir.path()).unwrap();
        let control = build_control_segment(&config(), &[]).unwrap();
        for segment in [&rootfs, &control] {
            assert_eq!(&segment[..2], &[0x1f, 0x8b]);
            assert!(segment.len() > 64);
        }
        assert_eq!(rootfs, build_rootfs_segment(dir.path()).unwrap());
        assert_eq!(control, build_control_segment(&config(), &[]).unwrap());
    }
}
