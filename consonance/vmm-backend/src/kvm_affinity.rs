// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeSet;

use crate::{BackendError, Result};

fn cpu_list(text: &str) -> Result<BTreeSet<u16>> {
    let invalid = || BackendError::Internal("invalid host CPU list");
    let mut cpus = BTreeSet::new();
    for part in text.trim().split(',') {
        let (first, last) = part.split_once('-').unwrap_or((part, part));
        let first: u16 = first.parse().map_err(|_| invalid())?;
        let last: u16 = last.parse().map_err(|_| invalid())?;
        if first > last {
            return Err(invalid());
        }
        cpus.extend(first..=last);
    }
    Ok(cpus)
}

fn check_masks(allowed: &str, pools: &[String]) -> Result<()> {
    let allowed = cpu_list(allowed)?;
    let pools = pools
        .iter()
        .map(|pool| cpu_list(pool))
        .collect::<Result<Vec<_>>>()?;
    if pools.iter().any(|pool| allowed.is_subset(pool)) {
        Ok(())
    } else {
        Err(BackendError::Unsupported {
            what: "mixed or unknown hybrid CPU affinity; use taskset/cpuset to select one core-type pool for all related executions and restores",
        })
    }
}

fn read_affinity_file(path: &str) -> std::io::Result<String> {
    #[cfg(test)]
    if let Some(result) = tests::fixture_read(path) {
        return result;
    }
    std::fs::read_to_string(path)
}

pub(crate) fn check_host_affinity() -> Result<()> {
    let mut pools = Vec::new();
    for kind in ["cpu_core", "cpu_atom"] {
        let path = format!("/sys/bus/event_source/devices/{kind}/cpus");
        match read_affinity_file(&path) {
            Ok(pool) if !pool.trim().is_empty() => pools.push(pool),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    if pools.is_empty() {
        return Ok(());
    }
    let status = read_affinity_file("/proc/thread-self/status")?;
    let allowed = status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
        .ok_or(BackendError::Internal("missing thread CPU affinity"))?;
    check_masks(allowed.trim(), &pools)
}

#[cfg(test)]
mod tests {
    use super::*;

    type Fixture =
        std::collections::BTreeMap<String, std::result::Result<String, std::io::ErrorKind>>;

    thread_local! {
        static FILES: std::cell::RefCell<Option<Fixture>> = const { std::cell::RefCell::new(None) };
    }

    pub(super) fn fixture_read(path: &str) -> Option<std::io::Result<String>> {
        FILES.with(|files| {
            files.borrow().as_ref().map(|files| {
                files
                    .get(path)
                    .expect("unexpected affinity file read")
                    .clone()
                    .map_err(std::io::Error::from)
            })
        })
    }

    fn with_files(
        core: std::result::Result<&str, std::io::ErrorKind>,
        atom: std::result::Result<&str, std::io::ErrorKind>,
        status: Option<&str>,
    ) -> Result<()> {
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                FILES.with(|files| *files.borrow_mut() = None);
            }
        }
        let mut files = Fixture::new();
        files.insert(
            "/sys/bus/event_source/devices/cpu_core/cpus".into(),
            core.map(str::to_owned),
        );
        files.insert(
            "/sys/bus/event_source/devices/cpu_atom/cpus".into(),
            atom.map(str::to_owned),
        );
        if let Some(status) = status {
            files.insert("/proc/thread-self/status".into(), Ok(status.to_owned()));
        }
        FILES.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(files);
        });
        let _clear = Clear;
        check_host_affinity()
    }

    #[test]
    fn absent_or_empty_sysfs_pools_admit_without_reading_thread_status() {
        use std::io::ErrorKind::NotFound;
        assert!(with_files(Err(NotFound), Err(NotFound), None).is_ok());
        assert!(with_files(Ok(""), Ok(" \n\t"), None).is_ok());
        assert!(with_files(Ok(" "), Err(NotFound), None).is_ok());
    }

    #[test]
    fn discovered_pool_enforces_actual_thread_affinity() {
        use std::io::ErrorKind::NotFound;
        for (core, atom) in [(Ok("0-7"), Err(NotFound)), (Ok("0-7"), Ok(" "))] {
            assert!(
                with_files(core, atom, Some("Name:\ttest\nCpus_allowed_list:\t0,2-3\n")).is_ok()
            );
            assert!(matches!(
                with_files(core, atom, Some("Cpus_allowed_list:\t0,8\n")),
                Err(BackendError::Unsupported { .. })
            ));
        }
        assert!(with_files(Err(NotFound), Ok("8-23"), Some("Cpus_allowed_list: 8,23")).is_ok());
        assert!(with_files(Ok("0-7"), Ok("8-23"), Some("Cpus_allowed_list: 0-23")).is_err());
    }

    #[test]
    fn sysfs_errors_and_missing_status_field_fail_closed() {
        use std::io::ErrorKind::{InvalidData, NotFound, PermissionDenied};
        assert!(with_files(Err(PermissionDenied), Err(NotFound), None).is_err());
        assert!(with_files(Err(NotFound), Err(InvalidData), None).is_err());
        assert!(matches!(
            with_files(Ok("0"), Err(NotFound), Some("Name: test\n")),
            Err(BackendError::Internal("missing thread CPU affinity"))
        ));
        assert!(with_files(Ok("malformed"), Err(NotFound), Some("Cpus_allowed_list: 0")).is_err());
    }

    #[test]
    fn affinity_requires_one_complete_core_type_pool() {
        let pools = vec!["0-7".to_owned(), "8-23".to_owned()];
        for allowed in ["0", "0-7", "1,3,5-7", "8-23", "8,20"] {
            assert!(check_masks(allowed, &pools).is_ok(), "{allowed}");
        }
        for allowed in ["0-23", "7-8", "0,20", "24", "8,24"] {
            assert!(
                matches!(
                    check_masks(allowed, &pools),
                    Err(BackendError::Unsupported { .. })
                ),
                "{allowed}"
            );
        }
    }

    #[test]
    fn malformed_topology_fails_closed() {
        for malformed in ["", "1-0", "1-", "1,,2", "-1", "65536", "0-999999999"] {
            assert!(cpu_list(malformed).is_err(), "{malformed}");
            assert!(
                check_masks("0", &[malformed.to_owned()]).is_err(),
                "{malformed}"
            );
        }
        assert!(check_masks("0", &["0,2-3".to_owned()]).is_ok());
        assert!(check_masks("0-3", &["0,2-3".to_owned()]).is_err());
    }
}
