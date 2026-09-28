// SPDX-License-Identifier: AGPL-3.0-or-later

pub const RESERVE_BYTES: u64 = 1 << 30;

#[must_use]
pub fn headroom_bytes() -> Option<u64> {
    host_available()
}

pub fn automatic_budget(workers: usize, worker_bytes: u64) -> Result<usize, String> {
    let headroom = headroom_bytes().ok_or("free memory is unknown on this host")?;
    let workers_bytes = worker_bytes.saturating_mul(workers as u64);
    let budget = headroom
        .saturating_sub(workers_bytes)
        .saturating_sub(RESERVE_BYTES);
    if budget == 0 {
        return Err(format!(
            "{} MiB free, {workers} workers need {} MiB and {} MiB stays in reserve",
            headroom >> 20,
            workers_bytes >> 20,
            RESERVE_BYTES >> 20
        ));
    }
    usize::try_from(budget).map_err(|_| "the budget does not fit this host".to_owned())
}

#[cfg(target_os = "linux")]
fn host_available() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let available = mem_available(&meminfo)?;
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").ok();
    let limit = cgroup
        .as_deref()
        .and_then(cgroup_path)
        .and_then(|path| cgroup_headroom(std::path::Path::new("/sys/fs/cgroup"), path, read));
    Some(limit.map_or(available, |limit| limit.min(available)))
}

#[cfg(target_os = "linux")]
fn read(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(any(target_os = "linux", test))]
fn mem_available(meminfo: &str) -> Option<u64> {
    let line = meminfo
        .lines()
        .find(|line| line.starts_with("MemAvailable:"))?;
    let kib: u64 = line.split_ascii_whitespace().nth(1)?.parse().ok()?;
    kib.checked_mul(1024)
}

#[cfg(any(target_os = "linux", test))]
fn cgroup_path(cgroup: &str) -> Option<&str> {
    cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(|path| path.trim_start_matches('/'))
}

#[cfg(any(target_os = "linux", test))]
fn cgroup_headroom(
    root: &std::path::Path,
    path: &str,
    read: impl Fn(&std::path::Path) -> Option<String>,
) -> Option<u64> {
    let mut headroom: Option<u64> = None;
    let mut dir = root.join(path);
    loop {
        let limit = read(&dir.join("memory.max")).and_then(|text| text.trim().parse::<u64>().ok());
        let usage =
            read(&dir.join("memory.current")).and_then(|text| text.trim().parse::<u64>().ok());
        if let (Some(limit), Some(usage)) = (limit, usage) {
            let here = limit.saturating_sub(usage);
            headroom = Some(headroom.map_or(here, |headroom| headroom.min(here)));
        }
        if dir == root || !dir.pop() || !dir.starts_with(root) {
            break;
        }
    }
    headroom
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
    use std::{collections::BTreeMap, path::PathBuf};

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    #[cfg_attr(miri, ignore = "reads the host's memory files")]
    fn this_host_reports_its_free_memory() {
        assert!(headroom_bytes().is_some_and(|bytes| bytes > 0));
    }

    #[test]
    fn meminfo_and_cgroup_lines_parse() {
        let meminfo = "MemTotal:  100 kB\nMemAvailable:   2048 kB\n";
        assert_eq!(mem_available(meminfo), Some(2 << 20));
        assert_eq!(mem_available("MemTotal: 1 kB\n"), None);
        assert_eq!(
            cgroup_path("0::/system.slice/run-1.scope\n"),
            Some("system.slice/run-1.scope")
        );
        assert_eq!(cgroup_path("1:cpu:/x\n"), None);
    }

    #[test]
    fn the_tightest_cgroup_on_the_path_bounds_headroom() {
        let root = PathBuf::from("/cg");
        let files = BTreeMap::from([
            (root.join("a/memory.max"), "1000\n"),
            (root.join("a/memory.current"), "400\n"),
            (root.join("a/b/memory.max"), "max\n"),
            (root.join("a/b/memory.current"), "300\n"),
            (root.join("a/b/c/memory.max"), "900\n"),
            (root.join("a/b/c/memory.current"), "100\n"),
        ]);
        let read = |path: &std::path::Path| files.get(path).map(|text| (*text).to_owned());
        assert_eq!(cgroup_headroom(&root, "a/b/c", read), Some(600));
        assert_eq!(cgroup_headroom(&root, "z", read), None);
    }

    #[test]
    #[cfg_attr(miri, ignore = "reads the host's memory files")]
    fn a_budget_needs_room_after_workers_and_the_reserve() {
        if let Some(headroom) = headroom_bytes() {
            assert!(automatic_budget(1, headroom).is_err());
        }
    }
}
