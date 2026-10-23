// SPDX-License-Identifier: AGPL-3.0-or-later

pub const VMSTAT_PATH: &str = "/proc/vmstat";
pub const OOM_POLL_TICKS: u64 = 100;

#[must_use]
pub fn oom_kills(vmstat: &str) -> Option<u64> {
    vmstat
        .lines()
        .find_map(|line| line.strip_prefix("oom_kill ")?.trim().parse().ok())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OomWatch {
    baseline: u64,
}

impl OomWatch {
    pub fn new(vmstat: &str) -> Result<Self, String> {
        Ok(Self {
            baseline: counter(vmstat)?,
        })
    }

    pub fn observe(self, vmstat: &str) -> Result<(), String> {
        let kills = counter(vmstat)?.saturating_sub(self.baseline);
        if kills == 0 {
            return Ok(());
        }
        Err(format!(
            "the guest kernel killed {kills} process{} for lack of memory; node disks and their \
             write logs live in guest memory, so raise ram_mib",
            if kills == 1 { "" } else { "es" }
        ))
    }
}

fn counter(vmstat: &str) -> Result<u64, String> {
    oom_kills(vmstat).ok_or_else(|| format!("{VMSTAT_PATH} has no oom_kill counter"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VMSTAT: &str = "nr_free_pages 1792\nnr_zone_write_pending 0\noom_kill 0\nnr_dirty 0\n";

    #[test]
    fn reads_the_oom_kill_counter() {
        assert_eq!(oom_kills(VMSTAT), Some(0));
        assert_eq!(oom_kills("pgfault 9\noom_kill 12\n"), Some(12));
        assert_eq!(oom_kills("oom_kill_total 3\n"), None);
        assert_eq!(oom_kills("oom_kill x\n"), None);
        assert_eq!(oom_kills(""), None);
    }

    #[test]
    fn a_kernel_without_the_counter_is_refused() {
        assert_eq!(
            OomWatch::new("pgfault 9\n"),
            Err("/proc/vmstat has no oom_kill counter".to_owned())
        );
    }

    #[test]
    fn kills_before_the_watch_started_are_ignored() {
        let watch = OomWatch::new("oom_kill 2\n").unwrap();
        assert_eq!(watch.observe("oom_kill 2\n"), Ok(()));
    }

    #[test]
    fn a_new_kill_fails_with_the_count() {
        let watch = OomWatch::new(VMSTAT).unwrap();
        let one = watch.observe("oom_kill 1\n").unwrap_err();
        assert!(one.starts_with("the guest kernel killed 1 process for lack of memory"));
        assert!(one.ends_with("raise ram_mib"));
        let three = watch.observe("oom_kill 3\n").unwrap_err();
        assert!(three.starts_with("the guest kernel killed 3 processes for lack of memory"));
    }

    #[test]
    fn a_vanished_counter_is_an_error() {
        let watch = OomWatch::new(VMSTAT).unwrap();
        assert!(watch.observe("pgfault 9\n").is_err());
    }
}
