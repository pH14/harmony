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

pub(crate) fn check_host_affinity() -> Result<()> {
    let mut pools = Vec::new();
    for kind in ["cpu_core", "cpu_atom"] {
        let path = format!("/sys/bus/event_source/devices/{kind}/cpus");
        match std::fs::read_to_string(path) {
            Ok(pool) if !pool.trim().is_empty() => pools.push(pool),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    if pools.is_empty() {
        return Ok(());
    }
    let status = std::fs::read_to_string("/proc/thread-self/status")?;
    let allowed = status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
        .ok_or(BackendError::Internal("missing thread CPU affinity"))?;
    check_masks(allowed.trim(), &pools)
}

#[cfg(test)]
mod tests {
    use super::*;

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
