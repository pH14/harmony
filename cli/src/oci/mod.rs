// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod runner;
use crate::config::{Backend, Config, Result};
use std::{fs, path::Path, time::Duration};

pub fn execute(
    c: &Config,
    kernel: &[u8],
    initramfs: &[u8],
    out: &Path,
    console: bool,
) -> Result<bool> {
    let cmdline = format!("{} {}", runner::cmdline(), c.knobs.join(" "));
    let spec = runner::RunSpec {
        kernel,
        initramfs,
        cmdline: &cmdline,
        guest_ram_len: (c.ram_mib as usize) << 20,
        seed: c.seed,
        wall_budget: Duration::from_secs(c.wall_seconds.unwrap_or(900)),
        stream: if console {
            runner::StreamMode::Full
        } else {
            runner::StreamMode::Container
        },
    };
    let mut uml_events = None;
    let outcome = if c.backend == Backend::Uml {
        let profile = uml::Profile::load(c.uml_profile.as_ref().ok_or("missing UML profile")?)?;
        let mut launch = uml::Launch::new(out.join("uml-work"));
        fs::create_dir_all(&launch.work_parent)?;
        launch.memory_mib = c.ram_mib;
        launch.bridge = Some(uml::Bridge::new(c.seed));
        launch.wall_limit = spec.wall_budget;
        launch.console_tail_bytes = 16 << 20;
        launch.console_limit_bytes = 16 << 20;
        launch.kernel_arguments = vec!["rdinit=/usr/lib/harmony/init".into()];
        launch.kernel_arguments.extend(c.knobs.iter().cloned());
        let temp = tempfile::NamedTempFile::new()?;
        fs::write(temp.path(), initramfs)?;
        launch.initramfs = Some(temp.path().to_path_buf());
        let exit = uml::Guest::spawn(&launch, &profile)?.wait()?;
        fs::write(out.join("serial.log"), &exit.console)?;
        if !matches!(exit.reason, uml::ExitReason::Exited(0)) || !exit.leftovers.is_empty() {
            return Err(format!(
                "UML execution stopped: {:?}; {} leftover processes",
                exit.reason,
                exit.leftovers.len()
            )
            .into());
        }
        uml_events = Some(uml::event_hash(&exit.events));
        runner::Outcome {
            serial: exit.console,
            steps: exit.events.len() as u64,
            reason: format!("{:?}", exit.reason),
        }
    } else {
        match runner::execute(&spec) {
            Ok(outcome) => outcome,
            Err(error) => {
                if let runner::RunError::WallBudget { serial, .. } = &error {
                    fs::write(out.join("serial.log"), serial)?;
                }
                return Err(error.into());
            }
        }
    };
    fs::write(out.join("serial.log"), &outcome.serial)?;
    let rc = parse_container_rc(&outcome.serial);
    let mut record = serde_json::json!({ "container_rc": rc, "supervisor_failure_rc": parse_supervisor_failure_rc(&outcome.serial),
        "runtime_rc": parse_runtime_rc(&outcome.serial), "startup_rc": parse_startup_rc(&outcome.serial),
        "serial_sha256": crate::runtime::digest(&outcome.serial), "steps": outcome.steps, "terminal": outcome.reason });
    if let Some(events) = uml_events {
        record["uml_events_sha256"] = events.into();
        record["application_sha256"] =
            crate::runtime::digest(application_output(&outcome.serial)?).into();
    }
    fs::write(out.join("run.json"), serde_json::to_vec_pretty(&record)?)?;
    if rc.is_none() || parse_supervisor_failure_rc(&outcome.serial).is_some() {
        return Err(format!("guest application did not start or its supervisor failed; inspect {}/serial.log (startup {:?}, supervisor {:?})", out.display(), parse_startup_rc(&outcome.serial), parse_supervisor_failure_rc(&outcome.serial)).into());
    }
    Ok(rc == Some(0))
}

fn application_output(serial: &[u8]) -> Result<&[u8]> {
    let start_marker = b"HARMONY_OCI: startup";
    let end_marker = b"\nHARMONY_OCI_EXIT rc=";
    let start = serial
        .windows(start_marker.len())
        .position(|v| v == start_marker)
        .ok_or("UML console has no application startup marker")?
        + start_marker.len();
    let start = start
        + serial[start..]
            .iter()
            .position(|b| *b == b'\n')
            .ok_or("incomplete UML startup marker")?
        + 1;
    let output = &serial[start..];
    let end = output
        .windows(end_marker.len())
        .rposition(|v| v == end_marker)
        .ok_or("UML console has no application completion marker")?;
    Ok(&output[..end])
}

pub fn equivalent_record(
    mut expected: serde_json::Value,
    mut actual: serde_json::Value,
    backend: Backend,
) -> bool {
    if backend == Backend::Uml {
        for record in [&mut expected, &mut actual] {
            if !record["application_sha256"].is_string() || !record["uml_events_sha256"].is_string()
            {
                return false;
            }
            let Some(object) = record.as_object_mut() else {
                return false;
            };
            object.remove("serial_sha256");
        }
    }
    expected == actual
}

fn parse_container_rc(serial: &[u8]) -> Option<i32> {
    parse_marker(serial, "HARMONY_OCI_APP_EXIT rc=")
}

fn parse_supervisor_failure_rc(serial: &[u8]) -> Option<i32> {
    parse_marker(serial, "HARMONY_OCI_SUPERVISOR_FAILURE rc=")
}

fn parse_runtime_rc(serial: &[u8]) -> Option<i32> {
    parse_marker(serial, "HARMONY_OCI_RUNTIME_EXIT rc=")
}

fn parse_startup_rc(serial: &[u8]) -> Option<i32> {
    parse_marker(serial, "HARMONY_OCI_STARTUP_EXIT rc=")
}

fn parse_marker(serial: &[u8], marker: &str) -> Option<i32> {
    let text = String::from_utf8_lossy(serial);
    text.lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(marker))
        .and_then(|rc| rc.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    #[test]
    fn uml_replay_checks_application_bytes_and_events_without_host_boot_noise() {
        let first = b"initrd=/tmp/one uml_dir=/tmp/host-one\nHARMONY_OCI: startup\nhello\nHARMONY_OCI_APP_EXIT rc=0\nHARMONY_OCI_EXIT rc=0\nhost shutdown one\n";
        let second = b"initrd=/tmp/two uml_dir=/tmp/host-two\nHARMONY_OCI: startup\nhello\nHARMONY_OCI_APP_EXIT rc=0\nHARMONY_OCI_EXIT rc=0\nhost shutdown two\n";
        assert_eq!(
            super::application_output(first).unwrap(),
            super::application_output(second).unwrap()
        );
        assert!(super::application_output(b"missing protocol").is_err());
        assert!(super::application_output(b"HARMONY_OCI: startup\nunfinished").is_err());
        let expected = serde_json::json!({"serial_sha256": crate::runtime::digest(first), "application_sha256": crate::runtime::digest(super::application_output(first).unwrap()), "uml_events_sha256": "events", "container_rc": 0});
        let mut actual = expected.clone();
        actual["serial_sha256"] = crate::runtime::digest(second).into();
        assert!(super::equivalent_record(
            expected.clone(),
            actual.clone(),
            crate::config::Backend::Uml
        ));
        assert!(!super::equivalent_record(
            expected.clone(),
            actual.clone(),
            crate::config::Backend::Kvm
        ));
        for field in ["application_sha256", "uml_events_sha256", "container_rc"] {
            let mut changed = actual.clone();
            changed[field] = "changed".into();
            assert!(!super::equivalent_record(
                expected.clone(),
                changed,
                crate::config::Backend::Uml
            ));
        }
        actual.as_object_mut().unwrap().remove("application_sha256");
        assert!(!super::equivalent_record(
            expected,
            actual,
            crate::config::Backend::Uml
        ));
    }
    #[test]
    fn container_rc_parses_last_marker() {
        let serial = b"noise\nHARMONY_OCI_APP_EXIT rc=3\ntail\nHARMONY_OCI_APP_EXIT rc=0\n";
        assert_eq!(super::parse_container_rc(serial), Some(0));
        assert_eq!(
            super::parse_container_rc(b"HARMONY_OCI_APP_EXIT rc=127\n"),
            Some(127)
        );
        assert_eq!(super::parse_container_rc(b"no marker"), None);
    }

    #[test]
    fn application_and_supervisor_failure_127_markers_are_distinct() {
        let application = b"HARMONY_OCI_APP_EXIT rc=127\nHARMONY_OCI_RUNTIME_EXIT rc=127\n";
        assert_eq!(super::parse_container_rc(application), Some(127));
        assert_eq!(super::parse_supervisor_failure_rc(application), None);

        let supervisor =
            b"HARMONY_OCI_SUPERVISOR_FAILURE rc=127\nHARMONY_OCI_RUNTIME_EXIT rc=127\n";
        assert_eq!(super::parse_container_rc(supervisor), None);
        assert_eq!(super::parse_supervisor_failure_rc(supervisor), Some(127));
    }

    #[test]
    fn runtime_and_startup_markers_parse_separately() {
        assert_eq!(
            super::parse_runtime_rc(b"HARMONY_OCI_RUNTIME_EXIT rc=1\n"),
            Some(1)
        );
        assert_eq!(
            super::parse_startup_rc(b"HARMONY_OCI_STARTUP_EXIT rc=125\n"),
            Some(125)
        );
        assert_eq!(super::parse_runtime_rc(b"unrelated output\n"), None);
        assert_eq!(
            super::parse_runtime_rc(b"HARMONY_OCI_RUNTIME_EXIT rc=0\n"),
            Some(0)
        );
        assert_eq!(
            super::parse_runtime_rc(b"HARMONY_OCI_RUNTIME_EXIT rc=127\n"),
            Some(127)
        );
    }

    #[test]
    fn platform_init_has_one_fixed_launch_and_separate_failure_markers() {
        let init = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../consonance/harmony-linux/runtime/init.sh"
        ));
        assert_eq!(
            init.matches("/usr/bin/runc run --no-pivot --bundle")
                .count(),
            1
        );
        assert!(init.contains("[ -x \"$RUNC\" ] || startup_failure 127"));
        assert!(init.contains("HARMONY_OCI_STARTUP_EXIT"));
        assert!(init.contains("HARMONY_OCI_RUNTIME_EXIT rc=$runc_status"));
        assert!(!init.contains("HARMONY_OCI_APP_EXIT rc=$runc_status"));
        assert!(!init.contains("chroot"));
        assert!(!init.contains("runc --version"));
    }
}
