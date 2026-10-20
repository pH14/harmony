// SPDX-License-Identifier: AGPL-3.0-or-later

mod topology;
pub use topology::CpuTopology;

#[cfg(any(target_os = "linux", test))]
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorePool {
    cpus: Vec<usize>,
    pinned: bool,
    topology: Vec<CpuTopology>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct Placement {
    pub coordinator: Option<usize>,
    pub workers: Vec<Option<usize>>,
}

impl CorePool {
    pub fn detect() -> Result<Self, String> {
        #[cfg(target_os = "linux")]
        {
            let allowed = allowed_cpus()?;
            let read_error = std::cell::RefCell::new(None);
            let read = |path: &str| {
                if read_error.borrow().is_some() {
                    return None;
                }
                let file = std::fs::File::open(path).ok()?;
                match bounded_cpu_text(file) {
                    Ok(text) => text,
                    Err(error) => {
                        read_error.replace(Some(format!("CPU metadata {path}: {error}")));
                        None
                    }
                }
            };
            let ranked = fastest_pool(&allowed, read);
            if let Some(error) = read_error.take() {
                return Err(error);
            }
            let ranked = ranked?;
            let topology = topology::census(&ranked, &read);
            if let Some(error) = read_error.take() {
                return Err(error);
            }
            let mut topology = topology?;
            let quota =
                crate::cgroup::here(|dir| crate::cgroup::cpu_cores(dir, &crate::cgroup::read))
                    .map(|quota| usize::try_from(quota).unwrap_or(usize::MAX));
            let cpus = topology::order(&ranked, &topology, quota);
            if cpus.is_empty() {
                return Err("this process may use no CPU".to_owned());
            }
            topology.retain(|info| cpus.contains(&info.cpu));
            Ok(Self {
                cpus,
                pinned: true,
                topology,
            })
        }
        #[cfg(target_os = "macos")]
        {
            let cores = performance_cores().or_else(|| {
                std::thread::available_parallelism()
                    .ok()
                    .map(std::num::NonZero::get)
            });
            Ok(Self {
                cpus: (0..cores.unwrap_or(1)).collect(),
                pinned: false,
                topology: Vec::new(),
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Err("worker placement supports Linux and macOS hosts".to_owned())
        }
    }

    #[must_use]
    pub fn cpus(&self) -> &[usize] {
        &self.cpus
    }

    #[must_use]
    pub fn topology(&self) -> &[CpuTopology] {
        &self.topology
    }

    #[must_use]
    pub fn max_workers(&self) -> usize {
        self.cpus.len().saturating_sub(1).max(1)
    }

    pub fn plan(&self, workers: u32) -> Result<Placement, String> {
        let workers = workers as usize;
        if workers == 0 || workers > self.max_workers() {
            return Err(format!(
                "{workers} workers do not fit {} cores of the fastest core type ({:?}), which leave room for {} workers and the coordinator",
                self.cpus.len(),
                self.cpus,
                self.max_workers()
            ));
        }
        if !self.pinned {
            return Ok(Placement {
                coordinator: None,
                workers: vec![None; workers],
            });
        }
        Ok(Placement {
            coordinator: self.cpus.last().copied(),
            workers: self.cpus[..workers].iter().copied().map(Some).collect(),
        })
    }
}

#[cfg(any(target_os = "linux", test))]
const MAX_CPU_LIST_BYTES: usize = 64 * 1024;
#[cfg(any(target_os = "linux", test))]
const MAX_CPU_LIST_ENTRIES: usize = 4096;

#[cfg(any(target_os = "linux", test))]
fn bounded_cpu_text(reader: impl std::io::Read) -> Result<Option<String>, String> {
    use std::io::Read;

    let mut bytes = Vec::new();
    if reader
        .take((MAX_CPU_LIST_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Ok(None);
    }
    if bytes.len() > MAX_CPU_LIST_BYTES {
        return Err("text exceeds the 64 KiB metadata limit".to_owned());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "metadata text is not UTF-8".to_owned())
}

#[cfg(any(target_os = "linux", test))]
fn cpu_ranges(text: &str) -> impl Iterator<Item = Option<std::ops::RangeInclusive<usize>>> + '_ {
    text.trim()
        .split(',')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (first, last) = part.split_once('-').unwrap_or((part, part));
            let first: usize = first.trim().parse().ok()?;
            let last: usize = last.trim().parse().ok()?;
            (first <= last).then_some(first..=last)
        })
}

#[cfg(any(target_os = "linux", test))]
fn cpu_list_count(text: &str, limit: usize) -> Option<usize> {
    if text.len() > MAX_CPU_LIST_BYTES {
        return None;
    }
    cpu_ranges(text).try_fold(0_usize, |count, range| {
        let range = range?;
        let size = range.end().checked_sub(*range.start())?.checked_add(1)?;
        let count = count.checked_add(size)?;
        (count <= limit).then_some(count)
    })
}

#[cfg(any(target_os = "linux", test))]
fn cpu_list(text: &str) -> Option<BTreeSet<usize>> {
    cpu_list_count(text, MAX_CPU_LIST_ENTRIES)?;
    let mut cpus = BTreeSet::new();
    for range in cpu_ranges(text) {
        cpus.extend(range?);
    }
    Some(cpus)
}

#[cfg(any(target_os = "linux", test))]
fn parse_number(text: &str) -> Option<u64> {
    let text = text.trim();
    match text.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

#[cfg(any(target_os = "linux", test))]
fn read_number(read: &impl Fn(&str) -> Option<String>, path: &str) -> Option<u64> {
    parse_number(&read(path)?)
}

#[cfg(any(target_os = "linux", test))]
fn core_types(
    allowed: &BTreeSet<usize>,
    read: &impl Fn(&str) -> Option<String>,
) -> Result<Vec<BTreeSet<usize>>, String> {
    let mut hybrid = Vec::new();
    for kind in ["cpu_core", "cpu_atom"] {
        if let Some(text) = read(&format!("/sys/bus/event_source/devices/{kind}/cpus")) {
            let pool = cpu_list(&text)
                .ok_or_else(|| format!("invalid or oversized CPU list for {kind}"))?;
            let pool = pool.intersection(allowed).copied().collect::<BTreeSet<_>>();
            if !pool.is_empty() {
                hybrid.push(pool);
            }
        }
    }
    if !hybrid.is_empty() {
        return Ok(hybrid);
    }
    let mut by_part: BTreeMap<Option<u64>, BTreeSet<usize>> = BTreeMap::new();
    for &cpu in allowed {
        let midr = read_number(
            read,
            &format!("/sys/devices/system/cpu/cpu{cpu}/regs/identification/midr_el1"),
        );
        by_part
            .entry(midr.map(|midr| midr & 0xff0f_fff0))
            .or_default()
            .insert(cpu);
    }
    Ok(by_part.into_values().collect())
}

#[cfg(any(target_os = "linux", test))]
fn fastest_pool(
    allowed: &BTreeSet<usize>,
    read: impl Fn(&str) -> Option<String>,
) -> Result<Vec<usize>, String> {
    let speed = |cpu: usize| {
        let base = format!("/sys/devices/system/cpu/cpu{cpu}");
        (
            read_number(&read, &format!("{base}/cpu_capacity")).unwrap_or(0),
            read_number(&read, &format!("{base}/cpufreq/cpuinfo_max_freq")).unwrap_or(0),
        )
    };
    let Some(pool) = core_types(allowed, &read)?.into_iter().max_by_key(|pool| {
        (
            pool.iter().map(|&cpu| speed(cpu)).max(),
            pool.len(),
            std::cmp::Reverse(pool.first().copied()),
        )
    }) else {
        return Ok(Vec::new());
    };
    let mut cpus: Vec<usize> = pool.into_iter().collect();
    cpus.sort_by_key(|&cpu| (std::cmp::Reverse(speed(cpu)), cpu));
    Ok(cpus)
}

#[cfg(target_os = "linux")]
fn allowed_cpus() -> Result<BTreeSet<usize>, String> {
    // SAFETY: cpu_set_t is a plain bit array for which all zero bits is the empty set.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    // SAFETY: set is a live cpu_set_t of the size passed; pid 0 names the calling thread.
    let rc =
        unsafe { libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &raw mut set) };
    if rc != 0 {
        return Err(format!(
            "read CPU affinity: {}",
            std::io::Error::last_os_error()
        ));
    }
    let limit = usize::try_from(libc::CPU_SETSIZE).unwrap_or(0);
    // SAFETY: every index is below CPU_SETSIZE, so CPU_ISSET reads inside set.
    Ok((0..limit)
        .filter(|&cpu| unsafe { libc::CPU_ISSET(cpu, &set) })
        .collect())
}

#[cfg(target_os = "linux")]
pub fn pin_current_thread(cpu: usize) -> Result<(), String> {
    if cpu >= usize::try_from(libc::CPU_SETSIZE).unwrap_or(0) {
        return Err(format!("CPU {cpu} is outside the affinity set"));
    }
    // SAFETY: cpu_set_t is a plain bit array for which all zero bits is the empty set.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    // SAFETY: cpu is below CPU_SETSIZE, so CPU_SET writes inside set.
    unsafe { libc::CPU_SET(cpu, &mut set) };
    // SAFETY: set is a live cpu_set_t of the size passed; pid 0 names the calling thread.
    let rc = unsafe {
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &raw const set)
    };
    if rc != 0 {
        return Err(format!(
            "pin thread to CPU {cpu}: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn pin_current_thread(cpu: usize) -> Result<(), String> {
    Err(format!("this host cannot pin a thread to CPU {cpu}"))
}

#[cfg(target_os = "macos")]
fn performance_cores() -> Option<usize> {
    let mut value: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is NUL-terminated, value and size are live locals of the
    // sizes given, and a null new value makes the call read-only.
    let rc = unsafe {
        libc::sysctlbyname(
            c"hw.perflevel0.logicalcpu".as_ptr(),
            (&raw mut value).cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && value > 0)
        .then(|| usize::try_from(value).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(entries: &[(String, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let files: BTreeMap<String, String> = entries
            .iter()
            .map(|(path, text)| (path.clone(), (*text).to_owned()))
            .collect();
        move |path| files.get(path).cloned()
    }

    fn arm_host(
        cores: &[(usize, &'static str, &'static str, &'static str)],
    ) -> Vec<(String, &'static str)> {
        cores
            .iter()
            .flat_map(|&(cpu, midr, capacity, freq)| {
                let base = format!("/sys/devices/system/cpu/cpu{cpu}");
                [
                    (format!("{base}/regs/identification/midr_el1"), midr),
                    (format!("{base}/cpu_capacity"), capacity),
                    (format!("{base}/cpufreq/cpuinfo_max_freq"), freq),
                ]
            })
            .collect()
    }

    fn arm_hybrid_host() -> Vec<(String, &'static str)> {
        let big = "0x00000000410fd811";
        let little = "0x00000000410fd801";
        arm_host(&[
            (0, big, "1024", "2600000"),
            (1, big, "1024", "2600000"),
            (2, little, "279", "1800000"),
            (3, little, "279", "1800000"),
            (4, little, "279", "1800000"),
            (5, little, "279", "1800000"),
            (6, big, "905", "2300000"),
            (7, big, "905", "2300000"),
            (8, big, "866", "2200000"),
            (9, big, "866", "2200000"),
            (10, big, "984", "2500000"),
            (11, big, "984", "2500000"),
        ])
    }

    #[test]
    fn cpu_lists_bound_expansion_without_bounding_sparse_identifiers() {
        let highest = usize::MAX;
        let first = highest - (MAX_CPU_LIST_ENTRIES - 1);
        let cpus = cpu_list(&format!("{first}-{highest}")).unwrap();
        assert_eq!(cpus.len(), MAX_CPU_LIST_ENTRIES);
        assert_eq!(cpus.first(), Some(&first));
        assert_eq!(cpus.last(), Some(&highest));
        assert_eq!(cpu_list(&format!("0,{highest}")), Some([0, highest].into()));
        for text in [
            "0-4294967295".to_owned(),
            format!("0-{highest}"),
            format!("{}-{highest}", first - 1),
            format!("{highest}-0"),
            format!("{highest}0"),
        ] {
            assert!(cpu_list(&text).is_none(), "{text}");
        }
    }

    #[test]
    fn cpu_lists_charge_overlap_and_literal_text_before_expansion() {
        let repeated = "0,".repeat(MAX_CPU_LIST_ENTRIES);
        assert_eq!(cpu_list(&repeated), Some([0].into()));
        assert!(cpu_list(&(repeated + "0")).is_none());
        let boundary = " ".repeat(MAX_CPU_LIST_BYTES - 1) + "0";
        assert_eq!(cpu_list(&boundary), Some([0].into()));
        assert!(cpu_list(&(boundary + " ")).is_none());
    }

    #[test]
    fn cpu_metadata_reader_stops_before_unbounded_text_allocation() {
        let boundary = vec![b'0'; MAX_CPU_LIST_BYTES];
        assert_eq!(
            bounded_cpu_text(boundary.as_slice())
                .unwrap()
                .unwrap()
                .len(),
            MAX_CPU_LIST_BYTES
        );
        let mut oversized = std::io::Cursor::new(vec![b'0'; MAX_CPU_LIST_BYTES + 2]);
        assert!(bounded_cpu_text(&mut oversized).is_err());
        assert_eq!(oversized.position(), (MAX_CPU_LIST_BYTES + 1) as u64);
        assert!(bounded_cpu_text([0xff].as_slice()).is_err());
        let mut invalid_oversized = std::io::Cursor::new(vec![0xff; MAX_CPU_LIST_BYTES + 2]);
        assert!(bounded_cpu_text(&mut invalid_oversized).is_err());
        assert_eq!(
            invalid_oversized.position(),
            (MAX_CPU_LIST_BYTES + 1) as u64
        );
    }

    #[test]
    fn arm_pool_is_the_fastest_core_type_ordered_fastest_first() {
        let pool = fastest_pool(&(0..12).collect(), files(&arm_hybrid_host())).unwrap();
        assert_eq!(pool, vec![0, 1, 10, 11, 6, 7, 8, 9]);
        let restricted = fastest_pool(&[2, 3, 4, 5].into(), files(&arm_hybrid_host())).unwrap();
        assert_eq!(restricted, vec![2, 3, 4, 5]);
    }

    #[test]
    fn hybrid_x86_pool_uses_the_perf_core_lists() {
        let read = files(&[
            (
                "/sys/bus/event_source/devices/cpu_core/cpus".to_owned(),
                "0-7\n",
            ),
            (
                "/sys/bus/event_source/devices/cpu_atom/cpus".to_owned(),
                "8-23\n",
            ),
            (
                "/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq".to_owned(),
                "5500000",
            ),
            (
                "/sys/devices/system/cpu/cpu8/cpufreq/cpuinfo_max_freq".to_owned(),
                "4600000",
            ),
        ]);
        assert_eq!(
            fastest_pool(&(0..24).collect(), &read).unwrap(),
            (0..8).collect::<Vec<_>>()
        );
        assert_eq!(
            fastest_pool(&(6..24).collect(), &read).unwrap(),
            (8..24).collect::<Vec<_>>()
        );
    }

    #[test]
    fn hybrid_lists_refuse_present_invalid_metadata_but_preserve_missing_and_empty_fallback() {
        let allowed = [1, 3].into();
        let path = "/sys/bus/event_source/devices/cpu_core/cpus";
        for text in ["", "9-10"] {
            assert_eq!(
                fastest_pool(&allowed, files(&[(path.to_owned(), text)])).unwrap(),
                [1, 3]
            );
        }
        assert_eq!(fastest_pool(&allowed, files(&[])).unwrap(), [1, 3]);
        for text in ["wrong", "0-4294967295", "3-1"] {
            assert!(
                fastest_pool(&allowed, files(&[(path.to_owned(), text)]))
                    .unwrap_err()
                    .contains("CPU list for cpu_core")
            );
        }
    }

    #[test]
    fn hosts_without_type_files_form_one_pool() {
        assert_eq!(
            fastest_pool(&[3, 1, 2].into(), files(&[])).unwrap(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn the_plan_keeps_one_core_for_the_coordinator_unless_only_one_is_allowed() {
        let pool = CorePool {
            cpus: vec![0, 1, 10, 11, 6, 7, 8, 9],
            pinned: true,
            topology: Vec::new(),
        };
        assert_eq!(pool.max_workers(), 7);
        let plan = pool.plan(7).expect("seven workers fit");
        assert_eq!(plan.coordinator, Some(9));
        assert_eq!(plan.workers, [0, 1, 10, 11, 6, 7, 8].map(Some).to_vec());
        assert!(pool.plan(8).is_err());
        assert!(pool.plan(0).is_err());
        let unpinned = CorePool {
            cpus: (0..8).collect(),
            pinned: false,
            topology: Vec::new(),
        };
        assert_eq!(
            unpinned.plan(4).expect("four workers fit").workers,
            vec![None; 4]
        );
        assert!(unpinned.plan(8).is_err());
        let single = CorePool {
            cpus: vec![3],
            pinned: true,
            topology: Vec::new(),
        };
        assert_eq!(single.max_workers(), 1);
        let plan = single.plan(1).expect("one worker shares the only core");
        assert_eq!((plan.coordinator, plan.workers), (Some(3), vec![Some(3)]));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_thread_pinned_to_an_allowed_cpu_reports_only_that_cpu() {
        let allowed = allowed_cpus().expect("affinity");
        let cpu = *allowed.iter().next().expect("one allowed CPU");
        std::thread::spawn(move || {
            pin_current_thread(cpu).expect("pin");
            assert_eq!(allowed_cpus().expect("affinity"), BTreeSet::from([cpu]));
        })
        .join()
        .expect("pinned thread");
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn macos_pool_counts_performance_cores() {
        let pool = CorePool::detect().expect("pool");
        assert!(!pool.cpus().is_empty());
        assert_eq!(pool.plan(1).map(|plan| plan.coordinator), Ok(None));
    }
}
