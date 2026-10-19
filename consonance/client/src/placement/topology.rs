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
pub(super) fn census(
    cpus: &[usize],
    read: &impl Fn(&str) -> Option<String>,
) -> Result<Vec<CpuTopology>, String> {
    use super::{cpu_list, read_number};

    let nodes = read("/sys/devices/system/node/online")
        .map(|text| cpu_list(&text).ok_or("invalid online NUMA node list"))
        .transpose()?
        .unwrap_or_default()
        .into_iter()
        .filter_map(|node| {
            read(&format!("/sys/devices/system/node/node{node}/cpulist"))
                .map(|text| (node, cpu_list(&text)))
        })
        .map(|(node, cpus)| {
            cpus.map(|cpus| (node, cpus))
                .ok_or_else(|| format!("invalid CPU list for NUMA node {node}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut raw = Vec::with_capacity(cpus.len());
    for &cpu in cpus {
        let base = format!("/sys/devices/system/cpu/cpu{cpu}");
        let siblings = read(&format!("{base}/topology/thread_siblings_list"))
            .map(|text| {
                let set =
                    cpu_list(&text).ok_or_else(|| format!("invalid sibling list for CPU {cpu}"))?;
                if !set.contains(&cpu) {
                    return Err(format!("CPU {cpu} is absent from its sibling list"));
                }
                Ok(set.into_iter().collect::<Vec<_>>())
            })
            .transpose()?;
        let mut last_cache = None;
        for index in 0..32 {
            let cache = format!("{base}/cache/index{index}");
            let Some(level) = read_number(read, &format!("{cache}/level")) else {
                continue;
            };
            if !read(&format!("{cache}/type"))
                .is_some_and(|kind| matches!(kind.trim(), "Data" | "Unified"))
            {
                continue;
            }
            let Some(shared) = read(&format!("{cache}/shared_cpu_list")) else {
                continue;
            };
            let shared = cpu_list(&shared)
                .filter(|shared| shared.contains(&cpu))
                .ok_or_else(|| format!("invalid shared cache list for CPU {cpu}"))?;
            if last_cache
                .as_ref()
                .is_none_or(|(previous, _)| level > *previous)
            {
                last_cache = Some((level, shared.into_iter().collect()));
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
                package: read_number(read, &format!("{base}/topology/physical_package_id")),
                core: read_number(read, &format!("{base}/topology/core_id")),
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
        let mut info = info.clone();
        for (_, siblings) in &raw {
            if let Some(siblings) = siblings
                && siblings.contains(&info.cpu)
            {
                if info.physical_core_known && info.siblings != *siblings {
                    return Err(format!("conflicting sibling lists for CPU {}", info.cpu));
                }
                info.siblings.clone_from(siblings);
                info.physical_core_known = true;
            }
        }
        if !info.physical_core_known {
            if let (Some(package), Some(core)) = (info.package, info.core) {
                info.siblings = raw
                    .iter()
                    .filter(|(peer, _)| peer.package == Some(package) && peer.core == Some(core))
                    .map(|(peer, _)| peer.cpu)
                    .collect();
                info.siblings.sort_unstable();
                info.physical_core_known = true;
            } else {
                info.siblings = vec![info.cpu];
            }
        }
        for (peer, _) in &raw {
            if info.siblings.contains(&peer.cpu)
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
            if (same_ids || info.siblings.contains(&peer.cpu)) && info.siblings != peer.siblings {
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
