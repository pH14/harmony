// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeSet;

use crate::{BackendError, HostCoreClass, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HostAffinity {
    Unreported,
    Reported {
        allowed: BTreeSet<u16>,
        class: HostCoreClass,
    },
}

impl HostAffinity {
    pub(crate) fn class(&self) -> Option<HostCoreClass> {
        match self {
            Self::Unreported => None,
            Self::Reported { class, .. } => Some(*class),
        }
    }
}

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

fn classify_allowed_mask(
    allowed: &BTreeSet<u16>,
    core: Option<&BTreeSet<u16>>,
    atom: Option<&BTreeSet<u16>>,
) -> Result<HostCoreClass> {
    match (
        core.is_some_and(|pool| allowed.is_subset(pool)),
        atom.is_some_and(|pool| allowed.is_subset(pool)),
    ) {
        (true, false) => Ok(HostCoreClass::Performance),
        (false, true) => Ok(HostCoreClass::Efficiency),
        _ => Err(BackendError::Unsupported {
            what: "mixed or unknown hybrid CPU affinity; use taskset/cpuset to select one core-type pool for all related executions and restores",
        }),
    }
}

#[cfg(test)]
fn check_masks(allowed: &str, pools: &[String]) -> Result<()> {
    let allowed = cpu_list(allowed)?;
    let pools = pools
        .iter()
        .map(|pool| cpu_list(pool))
        .collect::<Result<Vec<_>>>()?;
    classify_allowed_mask(&allowed, pools.first(), pools.get(1)).map(|_| ())
}

fn read_affinity_file(path: &str) -> std::io::Result<String> {
    #[cfg(test)]
    if let Some(result) = tests::fixture_read(path) {
        return result;
    }
    std::fs::read_to_string(path)
}

fn read_pool(kind: &str) -> Result<Option<BTreeSet<u16>>> {
    let path = format!("/sys/bus/event_source/devices/{kind}/cpus");
    match read_affinity_file(&path) {
        Ok(pool) if !pool.trim().is_empty() => Ok(Some(cpu_list(&pool)?)),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn host_reports_hybrid_architecture() -> bool {
    #[cfg(test)]
    if let Some(hybrid) = tests::fixture_hybrid() {
        return hybrid;
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        #[cfg(kani)]
        // SAFETY: x86-64 provides CPUID, and querying its maximum basic leaf has no memory effects.
        let maximum = unsafe { std::arch::x86_64::__cpuid(0) }.eax;
        #[cfg(not(kani))]
        let maximum = std::arch::x86_64::__cpuid(0).eax;
        if maximum < 7 {
            return false;
        }
        #[cfg(kani)]
        {
            // SAFETY: leaf 7 is supported above, and this constant subleaf query has no memory effects.
            unsafe { std::arch::x86_64::__cpuid_count(7, 0) }.edx & (1 << 15) != 0
        }
        #[cfg(not(kani))]
        {
            std::arch::x86_64::__cpuid_count(7, 0).edx & (1 << 15) != 0
        }
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        false
    }
}

pub(crate) fn check_host_affinity() -> Result<HostAffinity> {
    let core = read_pool("cpu_core")?;
    let atom = read_pool("cpu_atom")?;
    if core.is_none() && atom.is_none() {
        if host_reports_hybrid_architecture() {
            return Err(BackendError::Unsupported {
                what: "host reports hybrid x86 cores but core-type topology is unavailable",
            });
        }
        return Ok(HostAffinity::Unreported);
    }
    let status = read_affinity_file("/proc/thread-self/status")?;
    let allowed = status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
        .ok_or(BackendError::Internal("missing thread CPU affinity"))?;
    let allowed = cpu_list(allowed.trim())?;
    let class = classify_allowed_mask(&allowed, core.as_ref(), atom.as_ref())?;
    Ok(HostAffinity::Reported { allowed, class })
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(crate) fn ensure_host_affinity(expected: &HostAffinity) -> Result<()> {
    let HostAffinity::Reported { allowed, .. } = expected else {
        return Ok(());
    };
    let current = rustix::thread::sched_getaffinity(None).map_err(std::io::Error::from)?;
    check_allowed_mask(allowed, current.count() as usize, |cpu| {
        let cpu = usize::from(cpu);
        cpu < rustix::thread::CpuSet::MAX_CPU && current.is_set(cpu)
    })
}

#[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
pub(crate) const FULL_CHECK_PERIOD: u32 = 1024;

#[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
#[derive(Debug, Default)]
pub(crate) struct AffinityWatch {
    changed: std::cell::Cell<bool>,
    placements: std::cell::Cell<u32>,
}

#[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
impl AffinityWatch {
    pub(crate) fn full(
        &self,
        expected: &HostAffinity,
        check: impl FnOnce(&HostAffinity) -> Result<()>,
    ) -> Result<()> {
        if self.changed.get() {
            return Err(changed_affinity());
        }
        let result = check(expected);
        if result.is_err() {
            self.changed.set(true);
        }
        result
    }

    pub(crate) fn placement(
        &self,
        expected: &HostAffinity,
        current_cpu: impl FnOnce() -> usize,
        check: impl FnOnce(&HostAffinity) -> Result<()>,
    ) -> Result<()> {
        if self.changed.get() {
            return Err(changed_affinity());
        }
        let HostAffinity::Reported { allowed, .. } = expected else {
            return Ok(());
        };
        let placements = self.placements.get().wrapping_add(1) % FULL_CHECK_PERIOD;
        self.placements.set(placements);
        if placements != 0 && u16::try_from(current_cpu()).is_ok_and(|cpu| allowed.contains(&cpu)) {
            return Ok(());
        }
        self.full(expected, check)
    }
}

#[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
fn changed_affinity() -> BackendError {
    BackendError::Unsupported {
        what: "host CPU affinity changed after KVM backend creation",
    }
}

#[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
fn check_allowed_mask(
    allowed: &BTreeSet<u16>,
    count: usize,
    mut contains: impl FnMut(u16) -> bool,
) -> Result<()> {
    if count == allowed.len() && allowed.iter().copied().all(&mut contains) {
        Ok(())
    } else {
        Err(changed_affinity())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Fixture =
        std::collections::BTreeMap<String, std::result::Result<String, std::io::ErrorKind>>;

    thread_local! {
        static FILES: std::cell::RefCell<Option<Fixture>> = const { std::cell::RefCell::new(None) };
        static HYBRID: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
    }

    pub(super) fn fixture_hybrid() -> Option<bool> {
        HYBRID.with(std::cell::Cell::get)
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
    ) -> Result<HostAffinity> {
        with_files_and_hybrid(core, atom, status, false)
    }

    fn with_files_and_hybrid(
        core: std::result::Result<&str, std::io::ErrorKind>,
        atom: std::result::Result<&str, std::io::ErrorKind>,
        status: Option<&str>,
        hybrid: bool,
    ) -> Result<HostAffinity> {
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                FILES.with(|files| *files.borrow_mut() = None);
                HYBRID.with(|hybrid| hybrid.set(None));
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
        HYBRID.with(|slot| slot.set(Some(hybrid)));
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
    fn hybrid_cpuid_without_core_type_topology_fails_closed() {
        use std::io::ErrorKind::NotFound;
        for (core, atom) in [
            (Err(NotFound), Err(NotFound)),
            (Ok(""), Ok(" \n\t")),
            (Ok(" "), Err(NotFound)),
        ] {
            assert!(matches!(
                with_files_and_hybrid(core, atom, None, true),
                Err(BackendError::Unsupported { .. })
            ));
        }
    }

    #[test]
    fn discovered_pool_enforces_actual_thread_affinity() {
        use std::io::ErrorKind::NotFound;
        for (core, atom) in [(Ok("0-7"), Err(NotFound)), (Ok("0-7"), Ok(" "))] {
            assert_eq!(
                with_files(core, atom, Some("Name:\ttest\nCpus_allowed_list:\t0,2-3\n"))
                    .unwrap()
                    .class(),
                Some(HostCoreClass::Performance)
            );
            assert!(matches!(
                with_files(core, atom, Some("Cpus_allowed_list:\t0,8\n")),
                Err(BackendError::Unsupported { .. })
            ));
        }
        assert_eq!(
            with_files(Err(NotFound), Ok("8-23"), Some("Cpus_allowed_list: 8,23"))
                .unwrap()
                .class(),
            Some(HostCoreClass::Efficiency)
        );
        assert!(with_files(Ok("0-7"), Ok("8-23"), Some("Cpus_allowed_list: 0-23")).is_err());
        assert!(with_files(Ok("0-7"), Ok("0,8"), Some("Cpus_allowed_list: 0")).is_err());
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

    #[test]
    fn placement_checks_the_cpu_and_periodically_the_full_mask() {
        let expected = HostAffinity::Reported {
            allowed: cpu_list("8-9").unwrap(),
            class: HostCoreClass::Efficiency,
        };
        let watch = AffinityWatch::default();
        let mut full_checks = 0;
        for _ in 0..2 * FULL_CHECK_PERIOD {
            watch
                .placement(
                    &expected,
                    || 9,
                    |_| {
                        full_checks += 1;
                        Ok(())
                    },
                )
                .unwrap();
        }
        assert_eq!(full_checks, 2);
        watch
            .placement(
                &expected,
                || 3,
                |_| {
                    full_checks += 1;
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(full_checks, 3);
        assert!(
            AffinityWatch::default()
                .placement(&HostAffinity::Unreported, || 3, |_| unreachable!())
                .is_ok()
        );
    }

    #[test]
    fn a_failed_full_check_fails_every_later_check() {
        let expected = HostAffinity::Reported {
            allowed: cpu_list("8-9").unwrap(),
            class: HostCoreClass::Efficiency,
        };
        let watch = AffinityWatch::default();
        assert!(
            watch
                .placement(&expected, || 3, |_| Err(changed_affinity()))
                .is_err()
        );
        assert!(watch.placement(&expected, || 8, |_| Ok(())).is_err());
        assert!(watch.full(&expected, |_| Ok(())).is_err());
        assert!(
            watch
                .placement(&HostAffinity::Unreported, || 8, |_| Ok(()))
                .is_err()
        );
    }

    #[test]
    fn runtime_affinity_rejects_changed_mask() {
        let expected = cpu_list("0-1").unwrap();
        let equivalent = cpu_list("0,1").unwrap();
        assert!(
            check_allowed_mask(&expected, equivalent.len(), |cpu| equivalent.contains(&cpu))
                .is_ok()
        );
        for current in ["0", "1", "8-9", "0,8", "0-2"] {
            let current = cpu_list(current).unwrap();
            assert!(matches!(
                check_allowed_mask(&expected, current.len(), |cpu| current.contains(&cpu)),
                Err(BackendError::Unsupported { .. })
            ));
        }
    }
}
