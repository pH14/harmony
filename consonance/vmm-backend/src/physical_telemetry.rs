// SPDX-License-Identifier: AGPL-3.0-or-later

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunPlatform {
    KvmX86,
    KvmArm64,
    HvfArm64,
}

impl RunPlatform {
    const ALL: [Self; 3] = [Self::KvmX86, Self::KvmArm64, Self::HvfArm64];
    fn name(self) -> &'static str {
        match self {
            Self::KvmX86 => "kvm_x86",
            Self::KvmArm64 => "kvm_arm64",
            Self::HvfArm64 => "hvf_arm64",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunPurpose {
    Guest,
    Completion,
    Maintenance,
}

impl RunPurpose {
    const ALL: [Self; 3] = [Self::Guest, Self::Completion, Self::Maintenance];
    fn name(self) -> &'static str {
        match self {
            Self::Guest => "guest",
            Self::Completion => "completion",
            Self::Maintenance => "maintenance",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunReturn {
    Returned(u32),
    Interrupted,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalRunCost {
    pub entries: u64,
    pub returns: u64,
    pub interruptions: u64,
    pub errors: u64,
    pub nanos: u64,
    raw_exits: [u64; 65],
}

impl Default for PhysicalRunCost {
    fn default() -> Self {
        Self {
            entries: 0,
            returns: 0,
            interruptions: 0,
            errors: 0,
            nanos: 0,
            raw_exits: [0; 65],
        }
    }
}

impl PhysicalRunCost {
    pub fn raw_exit_count(&self, reason: u32) -> u64 {
        self.raw_exits[(reason as usize).min(64)]
    }
    fn merge(&mut self, other: &Self) {
        self.entries = self.entries.saturating_add(other.entries);
        self.returns = self.returns.saturating_add(other.returns);
        self.interruptions = self.interruptions.saturating_add(other.interruptions);
        self.errors = self.errors.saturating_add(other.errors);
        self.nanos = self.nanos.saturating_add(other.nanos);
        for (total, value) in self.raw_exits.iter_mut().zip(other.raw_exits) {
            *total = total.saturating_add(value);
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PhysicalTelemetry {
    sites: [[PhysicalRunCost; 3]; 3],
}

impl PhysicalTelemetry {
    pub fn cost(&self, platform: RunPlatform, purpose: RunPurpose) -> &PhysicalRunCost {
        &self.sites[platform as usize][purpose as usize]
    }
    pub fn merge(&mut self, other: &Self) {
        for (sites, incoming) in self.sites.iter_mut().zip(&other.sites) {
            for (site, value) in sites.iter_mut().zip(incoming) {
                site.merge(value);
            }
        }
    }
    #[allow(clippy::disallowed_methods)]
    pub fn measure<T>(
        &mut self,
        platform: RunPlatform,
        purpose: RunPurpose,
        enter: impl FnOnce() -> (T, RunReturn),
    ) -> T {
        let site = &mut self.sites[platform as usize][purpose as usize];
        site.entries = site.entries.saturating_add(1);
        let started = std::time::Instant::now();
        let (value, outcome) = enter();
        self.finish(platform, purpose, started.elapsed().as_nanos(), outcome);
        value
    }
    fn finish(
        &mut self,
        platform: RunPlatform,
        purpose: RunPurpose,
        elapsed: u128,
        outcome: RunReturn,
    ) {
        let site = &mut self.sites[platform as usize][purpose as usize];
        site.nanos = site
            .nanos
            .saturating_add(u64::try_from(elapsed).unwrap_or(u64::MAX));
        match outcome {
            RunReturn::Returned(reason) => {
                site.returns = site.returns.saturating_add(1);
                let bucket = &mut site.raw_exits[(reason as usize).min(64)];
                *bucket = bucket.saturating_add(1);
            }
            RunReturn::Interrupted => site.interruptions = site.interruptions.saturating_add(1),
            RunReturn::Error => site.errors = site.errors.saturating_add(1),
        }
    }
    pub fn counters(&self) -> Vec<(String, u64)> {
        let mut result = Vec::new();
        for platform in RunPlatform::ALL {
            for purpose in RunPurpose::ALL {
                let site = self.cost(platform, purpose);
                if site.entries == 0 {
                    continue;
                }
                let prefix = format!("backend.{}.{}", platform.name(), purpose.name());
                for (name, value) in [
                    ("entries", site.entries),
                    ("returns", site.returns),
                    ("interruptions", site.interruptions),
                    ("errors", site.errors),
                    ("run_ns", site.nanos),
                ] {
                    result.push((format!("{prefix}.{name}"), value));
                }
                for (reason, &value) in site
                    .raw_exits
                    .iter()
                    .enumerate()
                    .filter(|(_, value)| **value != 0)
                {
                    let reason = if reason == 64 {
                        "other".to_owned()
                    } else {
                        reason.to_string()
                    };
                    result.push((format!("{prefix}.raw_exit.{reason}.count"), value));
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn internally_consumed_returns_and_os_failures_remain_separate() {
        let mut telemetry = PhysicalTelemetry::default();
        let mut surfaced = 0;
        for (raw, consumed) in [
            (RunReturn::Returned(1), true),
            (RunReturn::Interrupted, true),
            (RunReturn::Returned(6), false),
            (RunReturn::Error, true),
        ] {
            let consumed =
                telemetry.measure(RunPlatform::KvmX86, RunPurpose::Guest, || (consumed, raw));
            surfaced += u64::from(!consumed);
        }
        let site = telemetry.cost(RunPlatform::KvmX86, RunPurpose::Guest);
        assert_eq!(
            (site.entries, site.returns, site.interruptions, site.errors),
            (4, 2, 1, 1)
        );
        assert_eq!(surfaced, 1);
        assert_eq!(site.raw_exit_count(1), 1);
        assert_eq!(site.raw_exit_count(6), 1);
        assert_eq!(site.raw_exit_count(0), 0);
    }
    #[test]
    fn purpose_platform_and_retirement_merges_are_disjoint_and_saturating() {
        let mut live = PhysicalTelemetry::default();
        live.measure(RunPlatform::HvfArm64, RunPurpose::Maintenance, || {
            ((), RunReturn::Returned(1))
        });
        live.measure(RunPlatform::KvmArm64, RunPurpose::Completion, || {
            ((), RunReturn::Interrupted)
        });
        let mut retired = PhysicalTelemetry::default();
        retired.merge(&live);
        assert_eq!(retired, live);
        assert_eq!(
            retired
                .cost(RunPlatform::HvfArm64, RunPurpose::Guest)
                .entries,
            0
        );
        assert_eq!(
            retired
                .cost(RunPlatform::HvfArm64, RunPurpose::Maintenance)
                .returns,
            1
        );
        assert_eq!(
            retired
                .cost(RunPlatform::KvmArm64, RunPurpose::Completion)
                .returns,
            0
        );
        let site = &mut retired.sites[0][0];
        site.entries = u64::MAX;
        site.nanos = u64::MAX;
        site.raw_exits[64] = u64::MAX;
        retired.finish(
            RunPlatform::KvmX86,
            RunPurpose::Guest,
            u128::MAX,
            RunReturn::Returned(u32::MAX),
        );
        assert_eq!(
            retired.cost(RunPlatform::KvmX86, RunPurpose::Guest).nanos,
            u64::MAX
        );
        assert_eq!(
            retired
                .cost(RunPlatform::KvmX86, RunPurpose::Guest)
                .raw_exit_count(64),
            u64::MAX
        );
        assert!(retired.counters().iter().any(|(key, value)| key
            == "backend.kvm_x86.guest.raw_exit.other.count"
            && *value == u64::MAX));
    }
}
