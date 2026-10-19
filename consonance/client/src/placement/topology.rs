// SPDX-License-Identifier: AGPL-3.0-or-later

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct CpuTopology {
    pub cpu: usize,
    pub package: Option<u64>,
    pub core: Option<u64>,
    pub siblings: Vec<usize>,
    pub physical_core_known: bool,
    pub last_level_cache: Option<Vec<usize>>,
    pub numa_node: Option<usize>,
}

#[cfg(any(target_os = "linux", test))]
const MAX_TOPOLOGY_CPUS: usize = 1024;
#[cfg(any(target_os = "linux", test))]
const MAX_TOPOLOGY_NODES: usize = 1024;

#[cfg(any(target_os = "linux", test))]
struct CensusBudget {
    text_remaining: usize,
    entries_remaining: usize,
}

#[cfg(any(target_os = "linux", test))]
impl CensusBudget {
    fn read(
        &mut self,
        read: &impl Fn(&str) -> Option<String>,
        path: &str,
    ) -> Result<Option<String>, String> {
        let text = read(path);
        if let Some(text) = &text {
            if text.len() > super::MAX_CPU_LIST_BYTES {
                return Err(format!("topology metadata exceeds the text limit: {path}"));
            }
            self.text_remaining = self
                .text_remaining
                .checked_sub(text.len())
                .ok_or("topology metadata exceeds the aggregate text budget")?;
        }
        Ok(text)
    }

    fn number(
        &mut self,
        read: &impl Fn(&str) -> Option<String>,
        path: &str,
    ) -> Result<Option<u64>, String> {
        Ok(self
            .read(read, path)?
            .and_then(|text| super::parse_number(&text)))
    }

    fn charge_entries(&mut self, count: usize) -> Result<(), String> {
        self.entries_remaining = self
            .entries_remaining
            .checked_sub(count)
            .ok_or("topology metadata exceeds the aggregate ID budget")?;
        Ok(())
    }

    fn list(
        &mut self,
        text: &str,
        limit: usize,
    ) -> Result<std::collections::BTreeSet<usize>, String> {
        let count =
            super::cpu_list_count(text, limit).ok_or("invalid or oversized topology ID list")?;
        self.charge_entries(count)?;
        super::cpu_list(text).ok_or_else(|| "invalid or oversized topology ID list".to_owned())
    }
}

#[cfg(any(target_os = "linux", test))]
pub(super) fn census(
    cpus: &[usize],
    read: &impl Fn(&str) -> Option<String>,
) -> Result<Vec<CpuTopology>, String> {
    census_with_budget(
        cpus,
        read,
        CensusBudget {
            text_remaining: 4 * 1024 * 1024,
            entries_remaining: 8 * 1024 * 1024,
        },
    )
}

#[cfg(any(target_os = "linux", test))]
fn census_with_budget(
    cpus: &[usize],
    read: &impl Fn(&str) -> Option<String>,
    mut budget: CensusBudget,
) -> Result<Vec<CpuTopology>, String> {
    if cpus.len() > MAX_TOPOLOGY_CPUS {
        return Err("topology exceeds the 1024 eligible CPU limit".to_owned());
    }
    let mut nodes = Vec::new();
    if let Some(text) = budget.read(read, "/sys/devices/system/node/online")? {
        let online = budget.list(&text, MAX_TOPOLOGY_NODES)?;
        for node in online {
            if let Some(text) = budget.read(
                read,
                &format!("/sys/devices/system/node/node{node}/cpulist"),
            )? {
                let members = budget
                    .list(&text, super::MAX_CPU_LIST_ENTRIES)
                    .map_err(|error| format!("NUMA node {node}: {error}"))?;
                nodes.push((node, members));
            }
        }
    }
    let mut raw = Vec::with_capacity(cpus.len());
    for &cpu in cpus {
        let base = format!("/sys/devices/system/cpu/cpu{cpu}");
        let siblings = if let Some(text) =
            budget.read(read, &format!("{base}/topology/thread_siblings_list"))?
        {
            let set = budget
                .list(&text, super::MAX_CPU_LIST_ENTRIES)
                .map_err(|error| format!("CPU {cpu} siblings: {error}"))?;
            if !set.contains(&cpu) {
                return Err(format!("CPU {cpu} is absent from its sibling list"));
            }
            budget.charge_entries(set.len())?;
            Some(set.into_iter().collect::<Vec<_>>())
        } else {
            None
        };
        let mut last_cache = None;
        for index in 0..32 {
            let cache = format!("{base}/cache/index{index}");
            let Some(level) = budget.number(read, &format!("{cache}/level"))? else {
                continue;
            };
            if !budget
                .read(read, &format!("{cache}/type"))?
                .is_some_and(|kind| matches!(kind.trim(), "Data" | "Unified"))
            {
                continue;
            }
            let Some(text) = budget.read(read, &format!("{cache}/shared_cpu_list"))? else {
                continue;
            };
            let shared = budget
                .list(&text, super::MAX_CPU_LIST_ENTRIES)
                .map_err(|error| format!("CPU {cpu} cache: {error}"))?;
            if !shared.contains(&cpu) {
                return Err(format!("invalid shared cache list for CPU {cpu}"));
            }
            if last_cache
                .as_ref()
                .is_none_or(|(previous, _)| level > *previous)
            {
                budget.charge_entries(shared.len())?;
                last_cache = Some((level, shared.into_iter().collect::<Vec<_>>()));
            }
        }
        let mut numa_node = None;
        for (node, members) in &nodes {
            if members.contains(&cpu) && numa_node.replace(*node).is_some() {
                return Err(format!("CPU {cpu} belongs to multiple NUMA nodes"));
            }
        }
        raw.push((
            CpuTopology {
                cpu,
                package: budget.number(read, &format!("{base}/topology/physical_package_id"))?,
                core: budget.number(read, &format!("{base}/topology/core_id"))?,
                siblings: Vec::new(),
                physical_core_known: false,
                last_level_cache: last_cache.map(|(_, cpus)| cpus),
                numa_node,
            },
            siblings,
        ));
    }
    let mut topology = Vec::with_capacity(raw.len());
    for (info, _) in &raw {
        budget.charge_entries(info.last_level_cache.as_ref().map_or(0, Vec::len))?;
        let mut info = info.clone();
        for (_, siblings) in &raw {
            if let Some(siblings) = siblings
                && siblings.binary_search(&info.cpu).is_ok()
            {
                if info.physical_core_known && info.siblings != *siblings {
                    return Err(format!("conflicting sibling lists for CPU {}", info.cpu));
                }
                if !info.physical_core_known {
                    budget.charge_entries(siblings.len())?;
                    info.siblings.clone_from(siblings);
                    info.physical_core_known = true;
                }
            }
        }
        if !info.physical_core_known {
            if let (Some(package), Some(core)) = (info.package, info.core) {
                let members = || {
                    raw.iter()
                        .filter(|(peer, _)| {
                            peer.package == Some(package) && peer.core == Some(core)
                        })
                        .map(|(peer, _)| peer.cpu)
                };
                budget.charge_entries(members().count())?;
                info.siblings = members().collect();
                info.siblings.sort_unstable();
                info.physical_core_known = true;
            } else {
                budget.charge_entries(1)?;
                info.siblings = vec![info.cpu];
            }
        }
        for (peer, _) in &raw {
            if info.siblings.binary_search(&peer.cpu).is_ok()
                && ((info.package.is_some()
                    && peer.package.is_some()
                    && info.package != peer.package)
                    || (info.core.is_some() && peer.core.is_some() && info.core != peer.core))
            {
                return Err(format!("conflicting core identifiers for CPU {}", info.cpu));
            }
        }
        topology.push(info);
    }
    for info in &topology {
        for peer in &topology {
            let same_ids = info.package.is_some()
                && info.core.is_some()
                && info.package == peer.package
                && info.core == peer.core;
            if (same_ids || info.siblings.binary_search(&peer.cpu).is_ok())
                && info.siblings != peer.siblings
            {
                return Err(format!(
                    "inconsistent physical core membership for CPU {}",
                    info.cpu
                ));
            }
        }
    }
    Ok(topology)
}

#[cfg(any(target_os = "linux", test))]
pub(super) fn order(
    ranked: &[usize],
    topology: &[CpuTopology],
    quota: Option<usize>,
) -> Vec<usize> {
    use std::collections::BTreeSet;

    let mut seen = BTreeSet::new();
    let mut primary = Vec::new();
    let mut siblings = Vec::new();
    for &cpu in ranked {
        let key = topology
            .iter()
            .find(|info| info.cpu == cpu)
            .map_or_else(|| vec![cpu], |info| info.siblings.clone());
        if seen.insert(key) {
            primary.push(cpu);
        } else {
            siblings.push(cpu);
        }
    }
    let mut ordered = primary.iter().copied().chain(siblings).collect::<Vec<_>>();
    if let Some(quota) = quota {
        ordered.truncate(quota);
    }
    if let Some(coordinator) = primary
        .iter()
        .rev()
        .find(|cpu| ordered.contains(cpu))
        .copied()
        && let Some(at) = ordered.iter().position(|cpu| *cpu == coordinator)
    {
        ordered.remove(at);
        ordered.push(coordinator);
    }
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn smt_files() -> BTreeMap<String, String> {
        let mut files = BTreeMap::new();
        for (cpu, siblings, core) in [
            (0, "0,4", "0"),
            (4, "0,4", "0"),
            (1, "1,5", "1"),
            (5, "1,5", "1"),
        ] {
            let base = format!("/sys/devices/system/cpu/cpu{cpu}");
            for (suffix, value) in [
                ("topology/thread_siblings_list", siblings),
                ("topology/physical_package_id", "0"),
                ("topology/core_id", core),
                ("cache/index0/level", "1"),
                ("cache/index0/type", "Data"),
                ("cache/index0/shared_cpu_list", siblings),
                ("cache/index2/level", "3"),
                ("cache/index2/type", "Unified"),
                ("cache/index2/shared_cpu_list", "0-1,4-5"),
                ("cache/index3/level", "4"),
                ("cache/index3/type", "Instruction"),
                ("cache/index3/shared_cpu_list", "0-1,4-5"),
            ] {
                files.insert(format!("{base}/{suffix}"), value.to_owned());
            }
        }
        files.insert(
            "/sys/devices/system/node/online".to_owned(),
            "0-1".to_owned(),
        );
        files.insert(
            "/sys/devices/system/node/node0/cpulist".to_owned(),
            "0,4".to_owned(),
        );
        files.insert(
            "/sys/devices/system/node/node1/cpulist".to_owned(),
            "1,5".to_owned(),
        );
        files
    }

    #[test]
    fn census_refuses_huge_compressed_lists_at_each_metadata_role() {
        for path in [
            "/sys/devices/system/node/online",
            "/sys/devices/system/node/node0/cpulist",
            "/sys/devices/system/cpu/cpu0/topology/thread_siblings_list",
            "/sys/devices/system/cpu/cpu0/cache/index2/shared_cpu_list",
        ] {
            let mut files = smt_files();
            files.insert(path.to_owned(), "0-4294967295".to_owned());
            let reads = std::cell::RefCell::new(Vec::new());
            let error = census(&[0], &|path| {
                reads.borrow_mut().push(path.to_owned());
                files.get(path).cloned()
            })
            .unwrap_err();
            assert!(error.contains("oversized"), "{path}: {error}");
            if path == "/sys/devices/system/node/online" {
                assert_eq!(*reads.borrow(), [path]);
            }
        }
    }

    #[test]
    fn census_bounds_fanout_and_preserves_sparse_high_identifiers() {
        let cpus = vec![0; MAX_TOPOLOGY_CPUS + 1];
        assert!(
            census(&cpus, &|_| panic!(
                "oversized eligible set must refuse before reads"
            ))
            .is_err()
        );
        let reads = std::cell::Cell::new(0);
        assert!(
            census(&[0], &|path| {
                reads.set(reads.get() + 1);
                assert_eq!(path, "/sys/devices/system/node/online");
                Some(format!("0-{MAX_TOPOLOGY_NODES}"))
            })
            .is_err()
        );
        assert_eq!(reads.get(), 1);
        let cpu = usize::MAX;
        let files = BTreeMap::from([
            (
                "/sys/devices/system/node/online".to_owned(),
                cpu.to_string(),
            ),
            (
                format!("/sys/devices/system/node/node{cpu}/cpulist"),
                cpu.to_string(),
            ),
            (
                format!("/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list"),
                format!("{}-{cpu}", cpu - 1),
            ),
            (
                format!("/sys/devices/system/cpu/cpu{cpu}/cache/index0/level"),
                "3".to_owned(),
            ),
            (
                format!("/sys/devices/system/cpu/cpu{cpu}/cache/index0/type"),
                "Unified".to_owned(),
            ),
            (
                format!("/sys/devices/system/cpu/cpu{cpu}/cache/index0/shared_cpu_list"),
                cpu.to_string(),
            ),
        ]);
        let topology = census(&[cpu], &|path| files.get(path).cloned()).unwrap();
        assert_eq!(topology[0].siblings, [cpu - 1, cpu]);
        assert_eq!(topology[0].last_level_cache, Some(vec![cpu]));
        assert_eq!(topology[0].numa_node, Some(cpu));
    }

    #[test]
    fn census_small_budget_counts_aggregate_node_expansion() {
        let files = BTreeMap::from([
            ("/sys/devices/system/node/online", "0,1"),
            ("/sys/devices/system/node/node0/cpulist", "0,2"),
            ("/sys/devices/system/node/node1/cpulist", "1,3"),
        ]);
        let read = |path: &str| files.get(path).map(|text| (*text).to_owned());
        assert!(
            census_with_budget(
                &[],
                &read,
                CensusBudget {
                    text_remaining: 9,
                    entries_remaining: 6,
                }
            )
            .unwrap()
            .is_empty()
        );
        let error = census_with_budget(
            &[],
            &read,
            CensusBudget {
                text_remaining: 9,
                entries_remaining: 5,
            },
        )
        .unwrap_err();
        assert!(error.contains("aggregate ID budget"), "{error}");
    }

    #[test]
    fn census_small_budgets_include_text_and_retained_copies() {
        let files = BTreeMap::from([
            (
                "/sys/devices/system/cpu/cpu0/topology/thread_siblings_list",
                "0",
            ),
            ("/sys/devices/system/cpu/cpu0/cache/index0/level", "3"),
            ("/sys/devices/system/cpu/cpu0/cache/index0/type", "Unified"),
            (
                "/sys/devices/system/cpu/cpu0/cache/index0/shared_cpu_list",
                "0",
            ),
        ]);
        let read = |path: &str| files.get(path).map(|text| (*text).to_owned());
        let topology = census_with_budget(
            &[0],
            &read,
            CensusBudget {
                text_remaining: 10,
                entries_remaining: 6,
            },
        )
        .unwrap();
        assert_eq!(topology[0].siblings, [0]);
        assert_eq!(topology[0].last_level_cache, Some(vec![0]));
        for (text_remaining, entries_remaining, expected) in [
            (9, 6, "aggregate text budget"),
            (10, 5, "aggregate ID budget"),
        ] {
            let error = census_with_budget(
                &[0],
                &read,
                CensusBudget {
                    text_remaining,
                    entries_remaining,
                },
            )
            .unwrap_err();
            assert!(error.contains(expected), "{error}");
        }
        assert!(
            census_with_budget(
                &[0],
                &|_| None,
                CensusBudget {
                    text_remaining: 0,
                    entries_remaining: 0,
                }
            )
            .unwrap_err()
            .contains("aggregate ID budget")
        );
    }

    #[test]
    fn census_charges_numeric_and_kind_text_as_well_as_lists() {
        for path in [
            "/sys/devices/system/cpu/cpu0/cache/index2/level",
            "/sys/devices/system/cpu/cpu0/cache/index2/type",
            "/sys/devices/system/cpu/cpu0/topology/physical_package_id",
            "/sys/devices/system/cpu/cpu0/topology/core_id",
        ] {
            let mut files = smt_files();
            files.insert(
                path.to_owned(),
                "0".repeat(super::super::MAX_CPU_LIST_BYTES + 1),
            );
            let error = census(&[0], &|path| files.get(path).cloned()).unwrap_err();
            assert!(error.contains("text limit"), "{path}: {error}");
        }
    }

    #[test]
    fn census_admits_1024_cpus_with_compressed_full_llc_membership() {
        let cpus = (0..MAX_TOPOLOGY_CPUS).collect::<Vec<_>>();
        let topology = census(&cpus, &|path| {
            if path == "/sys/devices/system/node/online" {
                return Some("0".to_owned());
            }
            if path == "/sys/devices/system/node/node0/cpulist" {
                return Some("0-1023".to_owned());
            }
            if let Some(base) = path.strip_suffix("/topology/thread_siblings_list") {
                return base
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .strip_prefix("cpu")
                    .map(str::to_owned);
            }
            match path
                .strip_prefix("/sys/devices/system/cpu/")?
                .split_once('/')?
                .1
            {
                "cache/index0/level" => Some("3".to_owned()),
                "cache/index0/type" => Some("Unified".to_owned()),
                "cache/index0/shared_cpu_list" => Some("0-1023".to_owned()),
                _ => None,
            }
        })
        .unwrap();
        assert_eq!(topology.len(), MAX_TOPOLOGY_CPUS);
        for (cpu, info) in topology.iter().enumerate() {
            assert_eq!(info.cpu, cpu);
            assert_eq!(info.siblings, [cpu]);
            assert_eq!(info.last_level_cache.as_ref().unwrap(), &cpus);
            assert_eq!(info.numa_node, Some(0));
        }
    }

    #[test]
    fn census_tracks_siblings_packages_last_data_cache_and_numa_nodes() {
        let files = smt_files();
        let topology = census(&[0, 4, 1, 5], &|path| files.get(path).cloned()).unwrap();
        for info in &topology {
            assert_eq!(info.package, Some(0));
            assert!(info.physical_core_known);
            assert_eq!(
                info.siblings,
                if info.cpu % 4 == 0 {
                    vec![0, 4]
                } else {
                    vec![1, 5]
                }
            );
            assert_eq!(info.numa_node, Some(info.cpu % 4));
            assert_eq!(info.last_level_cache, Some(vec![0, 1, 4, 5]));
        }
        assert_eq!(order(&[0, 4, 1, 5], &topology, None), [0, 4, 5, 1]);
        assert_eq!(order(&[0, 4, 1, 5], &topology, Some(2)), [0, 1]);
        assert_eq!(order(&[0, 4, 1, 5], &topology, Some(1)), [0]);
        assert!(order(&[0, 4, 1, 5], &topology, Some(0)).is_empty());
        let restricted = census(&[4, 5], &|path| files.get(path).cloned()).unwrap();
        assert_eq!(order(&[4, 5], &restricted, None), [4, 5]);
        let one_core = census(&[0, 4], &|path| files.get(path).cloned()).unwrap();
        assert_eq!(order(&[0, 4], &one_core, None), [4, 0]);
    }

    #[test]
    fn physical_representatives_precede_smt_and_coordinator_stays_fixed() {
        let topology = (0..6)
            .map(|cpu| CpuTopology {
                cpu,
                package: Some(0),
                core: Some((cpu % 3) as u64),
                siblings: vec![cpu % 3, cpu % 3 + 3],
                physical_core_known: true,
                last_level_cache: None,
                numa_node: Some(0),
            })
            .collect::<Vec<_>>();
        let cpus = order(&[0, 3, 1, 4, 2, 5], &topology, None);
        assert_eq!(cpus, [0, 1, 3, 4, 5, 2]);
        let pool = super::super::CorePool {
            cpus,
            pinned: true,
            topology,
        };
        for workers in 1..=pool.max_workers() {
            let plan = pool.plan(workers as u32).unwrap();
            assert_eq!(plan.coordinator, Some(2));
            assert!(!plan.workers.contains(&Some(2)));
            if workers <= 2 {
                assert_eq!(plan.workers, (0..workers).map(Some).collect::<Vec<_>>());
            }
        }
    }

    #[test]
    fn partial_sibling_files_inherit_known_groups_and_unknown_hosts_keep_speed_order() {
        let mut files = smt_files();
        files.remove("/sys/devices/system/cpu/cpu4/topology/thread_siblings_list");
        let topology = census(&[0, 4, 1, 5], &|path| files.get(path).cloned()).unwrap();
        assert_eq!(topology[0].siblings, topology[1].siblings);
        files.retain(|path, _| !path.ends_with("thread_siblings_list"));
        let topology = census(&[0, 4, 1, 5], &|path| files.get(path).cloned()).unwrap();
        assert_eq!(order(&[0, 4, 1, 5], &topology, None), [0, 4, 5, 1]);
        let unknown = census(&[9, 3, 7], &|_| None).unwrap();
        assert!(unknown.iter().all(|info| !info.physical_core_known));
        assert_eq!(order(&[9, 3, 7], &unknown, None), [9, 3, 7]);
        for cpu in [1, 5] {
            files.insert(
                format!("/sys/devices/system/cpu/cpu{cpu}/topology/core_id"),
                "0".to_owned(),
            );
            files.insert(
                format!("/sys/devices/system/cpu/cpu{cpu}/topology/physical_package_id"),
                "1".to_owned(),
            );
        }
        let packages = census(&[0, 4, 1, 5], &|path| files.get(path).cloned()).unwrap();
        assert_eq!(packages[0].siblings, [0, 4]);
        assert_eq!(packages[2].siblings, [1, 5]);
    }

    #[test]
    fn present_contradictory_topology_is_refused() {
        for (path, value) in [
            (
                "/sys/devices/system/cpu/cpu0/topology/thread_siblings_list",
                "4",
            ),
            (
                "/sys/devices/system/cpu/cpu4/topology/thread_siblings_list",
                "4",
            ),
            ("/sys/devices/system/cpu/cpu4/topology/core_id", "7"),
            (
                "/sys/devices/system/cpu/cpu4/topology/physical_package_id",
                "7",
            ),
            ("/sys/devices/system/node/node1/cpulist", "0-1,4-5"),
            (
                "/sys/devices/system/cpu/cpu0/cache/index2/shared_cpu_list",
                "1",
            ),
        ] {
            let mut files = smt_files();
            files.insert(path.to_owned(), value.to_owned());
            assert!(
                census(&[0, 4, 1, 5], &|path| files.get(path).cloned()).is_err(),
                "{path}"
            );
        }
    }
}
