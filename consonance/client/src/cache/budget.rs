// SPDX-License-Identifier: AGPL-3.0-or-later

pub const RESERVE_BYTES: u64 = 1 << 30;

#[must_use]
pub fn headroom_bytes() -> Option<u64> {
    host_available()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryNeeds {
    pub coordinator: u64,
    pub worker: u64,
    pub store_floor: u64,
    pub transient: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryPlan {
    pub workers: usize,
    pub budget: usize,
}

pub fn plan_memory(cores: usize, needs: MemoryNeeds) -> Result<MemoryPlan, String> {
    let headroom = headroom_bytes().ok_or("free memory is unknown on this host")?;
    fit(headroom, cores, needs)
}

fn fit(headroom: u64, cores: usize, needs: MemoryNeeds) -> Result<MemoryPlan, String> {
    let usable = headroom
        .saturating_sub(RESERVE_BYTES)
        .saturating_sub(needs.coordinator)
        .saturating_sub(needs.transient);
    let per_worker = needs.worker.saturating_add(needs.store_floor).max(1);
    let workers = (cores as u64).min(usable / per_worker);
    if workers == 0 {
        return Err(format!(
            "{} MiB free, one worker needs {} MiB, the coordinator {} MiB, a worker's transient \
             {} MiB, and {} MiB stays in reserve",
            headroom >> 20,
            per_worker >> 20,
            needs.coordinator >> 20,
            needs.transient >> 20,
            RESERVE_BYTES >> 20
        ));
    }
    let budget = usable.saturating_sub(workers.saturating_mul(needs.worker));
    Ok(MemoryPlan {
        workers: usize::try_from(workers).map_err(|_| "the worker count does not fit this host")?,
        budget: usize::try_from(budget).map_err(|_| "the budget does not fit this host")?,
    })
}

#[must_use]
pub fn measured_budget(planned: usize, counted: usize, headroom: u64) -> usize {
    let room = (counted as u64)
        .saturating_add(headroom)
        .saturating_sub(RESERVE_BYTES);
    planned.min(usize::try_from(room).unwrap_or(usize::MAX))
}

#[cfg(target_os = "linux")]
fn host_available() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let available = mem_available(&meminfo)?;
    let limit =
        crate::cgroup::here(|dir| crate::cgroup::memory_headroom(dir, &crate::cgroup::read));
    Some(limit.map_or(available, |limit| limit.min(available)))
}

#[cfg(any(target_os = "linux", test))]
fn mem_available(meminfo: &str) -> Option<u64> {
    let line = meminfo
        .lines()
        .find(|line| line.starts_with("MemAvailable:"))?;
    let kib: u64 = line.split_ascii_whitespace().nth(1)?.parse().ok()?;
    kib.checked_mul(1024)
}

#[cfg(target_os = "macos")]
fn host_available() -> Option<u64> {
    let pages = [
        "vm.page_free_count",
        "vm.page_pageable_external_count",
        "vm.page_purgeable_count",
    ]
    .into_iter()
    .map(sysctl_count)
    .try_fold(0u64, |total, count| total.checked_add(count?))?;
    pages.checked_mul(sysctl_count("hw.pagesize")?)
}

#[cfg(target_os = "macos")]
fn sysctl_count(name: &str) -> Option<u64> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut value = [0u8; 8];
    let mut len = value.len();
    // SAFETY: name is NUL-terminated and value is a live 8-byte buffer whose size is passed in
    // len.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    match (rc, len) {
        (0, 4) => Some(u64::from(u32::from_ne_bytes(value[..4].try_into().ok()?))),
        (0, 8) => Some(u64::from_ne_bytes(value)),
        _ => None,
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn host_available() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn needs(coordinator: u64, worker: u64, store_floor: u64) -> MemoryNeeds {
        MemoryNeeds {
            coordinator,
            worker,
            store_floor,
            transient: 0,
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    #[cfg_attr(miri, ignore = "reads the host's memory files")]
    fn this_host_reports_its_free_memory() {
        assert!(headroom_bytes().is_some_and(|bytes| bytes > 0));
    }

    #[test]
    fn meminfo_lines_parse() {
        let meminfo = "MemTotal:  100 kB\nMemAvailable:   2048 kB\n";
        assert_eq!(mem_available(meminfo), Some(2 << 20));
        assert_eq!(mem_available("MemTotal: 1 kB\n"), None);
    }

    #[test]
    fn workers_fit_guests_and_store_floors_and_the_budget_takes_the_rest() {
        let plan = fit(12 * GIB, 8, needs(0, 2 * GIB, GIB)).expect("plan");
        assert_eq!(plan.workers, 3);
        assert_eq!(plan.budget as u64, 11 * GIB - 3 * 2 * GIB);
        let plan = fit(40 * GIB, 7, needs(0, 2 * GIB, GIB)).expect("plan");
        assert_eq!(plan.workers, 7);
        assert_eq!(plan.budget as u64, 39 * GIB - 7 * 2 * GIB);
        assert!(fit(3 * GIB, 8, needs(0, 2 * GIB, GIB)).is_err());
    }

    #[test]
    fn the_coordinator_comes_out_before_the_workers() {
        let plan = fit(12 * GIB, 8, needs(2 * GIB, 2 * GIB, GIB)).expect("plan");
        assert_eq!(plan.workers, 3);
        assert_eq!(plan.budget as u64, 9 * GIB - 3 * 2 * GIB);
        let error = fit(4 * GIB, 8, needs(2 * GIB, 2 * GIB, GIB)).unwrap_err();
        assert!(error.contains("the coordinator 2048 MiB"), "{error}");
    }

    #[test]
    fn one_transient_comes_out_before_the_workers() {
        let plan = fit(
            12 * GIB,
            8,
            MemoryNeeds {
                transient: 2 * GIB,
                ..needs(0, 2 * GIB, GIB)
            },
        )
        .expect("plan");
        assert_eq!(plan.workers, 3);
        assert_eq!(plan.budget as u64, 9 * GIB - 3 * 2 * GIB);
        let error = fit(
            5 * GIB,
            8,
            MemoryNeeds {
                transient: 2 * GIB,
                ..needs(0, 2 * GIB, GIB)
            },
        )
        .unwrap_err();
        assert!(error.contains("a worker's transient 2048 MiB"), "{error}");
    }

    #[test]
    fn measured_memory_sets_the_budget_up_to_the_plan() {
        let planned = (6 * GIB) as usize;
        assert_eq!(measured_budget(planned, 0, 20 * GIB), planned);
        assert_eq!(
            measured_budget(planned, (4 * GIB) as usize, 2 * GIB) as u64,
            5 * GIB
        );
        assert_eq!(
            measured_budget(planned, (4 * GIB) as usize, 3 * GIB),
            planned
        );
        assert_eq!(measured_budget(planned, 0, GIB / 2), 0);
    }

    fn cgroup_headroom(max: u64, current: u64, stat: &str) -> u64 {
        let files = [
            ("/cg/memory.max", max.to_string()),
            ("/cg/memory.current", current.to_string()),
            ("/cg/memory.stat", stat.to_owned()),
        ];
        let read = |path: &std::path::Path| {
            files
                .iter()
                .find(|(name, _)| std::path::Path::new(name) == path)
                .map(|(_, text)| text.clone())
        };
        crate::cgroup::memory_headroom(std::path::Path::new("/cg"), &read).expect("headroom")
    }

    #[test]
    fn page_cache_left_by_a_build_keeps_every_worker() {
        let needs = needs(GIB / 2, GIB + GIB / 4, GIB + GIB / 2);
        let empty = cgroup_headroom(14 * GIB, 0, "file 0\nshmem 0\n");
        let built = cgroup_headroom(
            14 * GIB,
            10 * GIB,
            "anon 0\nfile 10737418240\nshmem 0\nfile_dirty 0\nfile_writeback 0\n",
        );
        assert_eq!(built, empty);
        let plan = fit(built, 5, needs).expect("plan");
        assert_eq!(plan.workers, 4);
        let dirty = cgroup_headroom(
            14 * GIB,
            10 * GIB,
            "file 10737418240\nshmem 2147483648\nfile_dirty 1073741824\nfile_writeback 0\n",
        );
        assert_eq!(dirty, 11 * GIB);
    }

    #[test]
    fn fourteen_twelve_and_ten_gib_cgroups_keep_guests_out_of_the_budget() {
        let worker = GIB + GIB / 4;
        let faults = MemoryNeeds {
            transient: GIB,
            ..needs(GIB / 2, worker, GIB)
        };
        let fixed = RESERVE_BYTES + GIB / 2 + GIB;
        let headroom = cgroup_headroom(14 * GIB, 0, "file 0\n");
        let plan = fit(headroom, 6, faults).expect("plan");
        assert_eq!(plan.workers, 5);
        assert_eq!(plan.budget as u64, 14 * GIB - fixed - 5 * worker);
        let plan = fit(cgroup_headroom(14 * GIB - GIB / 2, 0, ""), 6, faults).expect("plan");
        assert_eq!(plan.workers, 4);
        assert_eq!(plan.budget as u64, 14 * GIB - GIB / 2 - fixed - 4 * worker);
        let plan = fit(cgroup_headroom(12 * GIB, 0, ""), 5, faults).expect("plan");
        assert_eq!(plan.workers, 4);
        assert_eq!(plan.budget as u64, 12 * GIB - fixed - 4 * worker);
        let plan = fit(cgroup_headroom(10 * GIB, 0, ""), 4, faults).expect("plan");
        assert_eq!(plan.workers, 3);
        assert_eq!(plan.budget as u64, 10 * GIB - fixed - 3 * worker);
    }

    #[test]
    #[cfg_attr(miri, ignore = "reads the host's memory files")]
    fn a_plan_needs_room_for_one_worker_after_the_reserve() {
        if let Some(headroom) = headroom_bytes() {
            assert!(plan_memory(1, needs(0, headroom, 0)).is_err());
        }
    }
}
