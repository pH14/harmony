// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use vmm_backend::{ExitReason, StoreCompletions};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ExitSite {
    pub reason: ExitReason,
    pub site: u64,
    pub write: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Cost {
    pub count: u64,
    pub nanos: u64,
}

impl Cost {
    pub fn add(&mut self, nanos: u128) {
        self.count = self.count.saturating_add(1);
        self.nanos = self
            .nanos
            .saturating_add(u64::try_from(nanos).unwrap_or(u64::MAX));
    }

    fn merge(&mut self, other: Cost) {
        self.count = self.count.saturating_add(other.count);
        self.nanos = self.nanos.saturating_add(other.nanos);
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExitTelemetry {
    pub guest_runs: Cost,
    pub exits: BTreeMap<ExitSite, Cost>,
    pub store_completions: StoreCompletions,
}

impl ExitTelemetry {
    pub fn merge(&mut self, other: &ExitTelemetry) {
        self.guest_runs.merge(other.guest_runs);
        for (site, cost) in &other.exits {
            self.exits.entry(*site).or_default().merge(*cost);
        }
        self.store_completions.runs = self
            .store_completions
            .runs
            .saturating_add(other.store_completions.runs);
        self.store_completions.nanos = self
            .store_completions
            .nanos
            .saturating_add(other.store_completions.nanos);
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HostTelemetry {
    pub exits: ExitTelemetry,
    pub seals: Cost,
    pub seal_pages: u64,
    pub restores: Cost,
    pub restore_pages: u64,
    pub restore_fallbacks: u64,
    pub exports: Cost,
    pub export_pages: u64,
    pub imports: Cost,
    pub import_pages: u64,
}

impl HostTelemetry {
    pub fn counters(&self) -> Vec<(String, u64)> {
        let mut out = vec![
            ("guest.runs".to_owned(), self.exits.guest_runs.count),
            ("guest.run_ns".to_owned(), self.exits.guest_runs.nanos),
            (
                "guest.store_completions".to_owned(),
                self.exits.store_completions.runs,
            ),
            (
                "guest.store_completion_ns".to_owned(),
                self.exits.store_completions.nanos,
            ),
            ("snapshot.seals".to_owned(), self.seals.count),
            ("snapshot.seal_ns".to_owned(), self.seals.nanos),
            ("snapshot.seal_pages".to_owned(), self.seal_pages),
            ("snapshot.restores".to_owned(), self.restores.count),
            ("snapshot.restore_ns".to_owned(), self.restores.nanos),
            ("snapshot.restore_pages".to_owned(), self.restore_pages),
            (
                "snapshot.restore_fallbacks".to_owned(),
                self.restore_fallbacks,
            ),
            ("snapshot.exports".to_owned(), self.exports.count),
            ("snapshot.export_ns".to_owned(), self.exports.nanos),
            ("snapshot.export_pages".to_owned(), self.export_pages),
            ("snapshot.imports".to_owned(), self.imports.count),
            ("snapshot.import_ns".to_owned(), self.imports.nanos),
            ("snapshot.import_pages".to_owned(), self.import_pages),
        ];
        let mut total = Cost::default();
        for (site, cost) in &self.exits.exits {
            total.merge(*cost);
            let name = format!(
                "exit.{:?}.{:#x}.{}",
                site.reason,
                site.site,
                if site.write { "write" } else { "read" }
            )
            .to_lowercase();
            out.push((format!("{name}.count"), cost.count));
            out.push((format!("{name}.ns"), cost.nanos));
        }
        out.push(("exit.count".to_owned(), total.count));
        out.push(("exit.ns".to_owned(), total.nanos));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_exit_telemetry_sums_every_site() {
        let site = ExitSite {
            reason: ExitReason::Mmio,
            site: 0x20,
            write: true,
        };
        let mut first = ExitTelemetry::default();
        first.exits.entry(site).or_default().add(5);
        first.guest_runs.add(7);
        let mut second = ExitTelemetry::default();
        second.exits.entry(site).or_default().add(11);
        second.store_completions.runs = 2;
        first.merge(&second);
        assert_eq!(
            first.exits[&site],
            Cost {
                count: 2,
                nanos: 16
            }
        );
        assert_eq!(first.guest_runs, Cost { count: 1, nanos: 7 });
        assert_eq!(first.store_completions.runs, 2);
        let counters = HostTelemetry {
            exits: first,
            ..HostTelemetry::default()
        }
        .counters();
        assert!(counters.contains(&("exit.mmio.0x20.write.count".to_owned(), 2)));
        assert!(counters.contains(&("exit.count".to_owned(), 2)));
    }
}
