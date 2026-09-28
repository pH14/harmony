// SPDX-License-Identifier: AGPL-3.0-or-later

pub const RESERVE_BYTES: u64 = 1 << 30;

#[must_use]
pub fn headroom_bytes() -> Option<u64> {
    host_available()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryPlan {
    pub workers: usize,
    pub budget: usize,
}

pub fn plan_memory(
    cores: usize,
    guest_bytes: u64,
    store_floor_bytes: u64,
) -> Result<MemoryPlan, String> {
    let headroom = headroom_bytes().ok_or("free memory is unknown on this host")?;
    fit(headroom, cores, guest_bytes, store_floor_bytes)
}

fn fit(
    headroom: u64,
    cores: usize,
    guest_bytes: u64,
    store_floor_bytes: u64,
) -> Result<MemoryPlan, String> {
    let usable = headroom.saturating_sub(RESERVE_BYTES);
    let per_worker = guest_bytes.saturating_add(store_floor_bytes).max(1);
    let workers = (cores as u64).min(usable / per_worker);
    if workers == 0 {
        return Err(format!(
            "{} MiB free, one worker needs {} MiB and {} MiB stays in reserve",
            headroom >> 20,
            per_worker >> 20,
            RESERVE_BYTES >> 20
        ));
    }
    let budget = usable.saturating_sub(workers.saturating_mul(guest_bytes));
    Ok(MemoryPlan {
        workers: usize::try_from(workers).map_err(|_| "the worker count does not fit this host")?,
        budget: usize::try_from(budget).map_err(|_| "the budget does not fit this host")?,
    })
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
        const GIB: u64 = 1 << 30;
        let plan = fit(12 * GIB, 8, 2 * GIB, GIB).expect("plan");
        assert_eq!(plan.workers, 3);
        assert_eq!(plan.budget as u64, 11 * GIB - 3 * 2 * GIB);
        let plan = fit(40 * GIB, 7, 2 * GIB, GIB).expect("plan");
        assert_eq!(plan.workers, 7);
        assert_eq!(plan.budget as u64, 39 * GIB - 7 * 2 * GIB);
        assert!(fit(3 * GIB, 8, 2 * GIB, GIB).is_err());
    }

    #[test]
    #[cfg_attr(miri, ignore = "reads the host's memory files")]
    fn a_plan_needs_room_for_one_worker_after_the_reserve() {
        if let Some(headroom) = headroom_bytes() {
            assert!(plan_memory(1, headroom, 0).is_err());
        }
    }
}
