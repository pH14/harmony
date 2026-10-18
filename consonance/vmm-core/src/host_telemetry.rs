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

#[derive(Clone, Copy, Debug)]
pub(crate) enum HashSite {
    WholeRequest,
    Checkpoint,
    PortableExport,
    PortableImport,
    SparseBase,
    PortableExportEnvelope,
    PortableImportEnvelope,
}

impl HashSite {
    const ALL: [Self; 7] = [
        Self::WholeRequest,
        Self::Checkpoint,
        Self::PortableExport,
        Self::PortableImport,
        Self::SparseBase,
        Self::PortableExportEnvelope,
        Self::PortableImportEnvelope,
    ];
    fn name(self) -> &'static str {
        match self {
            Self::WholeRequest => "whole_request",
            Self::Checkpoint => "checkpoint",
            Self::PortableExport => "portable_export",
            Self::PortableImport => "portable_import",
            Self::SparseBase => "sparse_base",
            Self::PortableExportEnvelope => "portable_export_envelope",
            Self::PortableImportEnvelope => "portable_import_envelope",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct HashCost {
    pub calls: u64,
    pub ram_bytes: u64,
    pub suffix_bytes: u64,
    pub metadata_bytes: u64,
    pub nanos: u64,
    pub errors: u64,
}

impl HashCost {
    fn merge(&mut self, other: Self) {
        self.calls = self.calls.saturating_add(other.calls);
        self.ram_bytes = self.ram_bytes.saturating_add(other.ram_bytes);
        self.suffix_bytes = self.suffix_bytes.saturating_add(other.suffix_bytes);
        self.metadata_bytes = self.metadata_bytes.saturating_add(other.metadata_bytes);
        self.nanos = self.nanos.saturating_add(other.nanos);
        self.errors = self.errors.saturating_add(other.errors);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HashTelemetry {
    costs: [HashCost; 7],
}

impl HashTelemetry {
    #[cfg(test)]
    pub(crate) fn cost(&self, site: HashSite) -> HashCost {
        self.costs[site as usize]
    }
    pub fn merge(&mut self, other: &Self) {
        for (cost, other) in self.costs.iter_mut().zip(other.costs) {
            cost.merge(other);
        }
    }
    fn counters(&self, out: &mut Vec<(String, u64)>) {
        for site in HashSite::ALL {
            let cost = self.costs[site as usize];
            for (name, value) in [
                ("calls", cost.calls),
                ("ram_bytes", cost.ram_bytes),
                ("suffix_bytes", cost.suffix_bytes),
                ("metadata_bytes", cost.metadata_bytes),
                ("ns", cost.nanos),
                ("errors", cost.errors),
            ] {
                out.push((format!("hash.{}.{name}", site.name()), value));
            }
        }
    }
}

pub(crate) struct HashWork {
    started: std::time::Instant,
    cost: HashCost,
}

impl HashWork {
    #[allow(clippy::disallowed_methods)]
    pub(crate) fn new() -> Self {
        Self {
            started: std::time::Instant::now(),
            cost: HashCost::default(),
        }
    }
    pub(crate) fn ram(&mut self, bytes: usize) {
        self.cost.ram_bytes = self
            .cost
            .ram_bytes
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
    }
    pub(crate) fn suffix(&mut self, bytes: usize) {
        self.cost.suffix_bytes = self
            .cost
            .suffix_bytes
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
    }
    pub(crate) fn metadata(&mut self, bytes: usize) {
        self.cost.metadata_bytes = self
            .cost
            .metadata_bytes
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
    }
    pub(crate) fn finish(
        mut self,
        telemetry: &std::cell::Cell<HashTelemetry>,
        site: HashSite,
        failed: bool,
    ) {
        self.cost.calls = 1;
        self.cost.errors = u64::from(failed);
        self.cost.nanos = u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let mut total = telemetry.get();
        total.costs[site as usize].merge(self.cost);
        telemetry.set(total);
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HostTelemetry {
    pub exits: ExitTelemetry,
    pub hashes: HashTelemetry,
    pub verification: snapshot_store::VerificationTelemetry,
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
        self.hashes.counters(&mut out);
        out.extend(self.verification.counters());
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
    fn hash_totals_saturate_and_export_disjoint_site_names() {
        let mut telemetry = HashTelemetry::default();
        telemetry.costs[HashSite::WholeRequest as usize] = HashCost {
            calls: u64::MAX,
            ram_bytes: u64::MAX,
            suffix_bytes: u64::MAX,
            metadata_bytes: u64::MAX,
            nanos: u64::MAX,
            errors: u64::MAX,
        };
        let mut incoming = HashTelemetry::default();
        incoming.costs[HashSite::WholeRequest as usize] = HashCost {
            calls: 1,
            ram_bytes: 1,
            suffix_bytes: 1,
            metadata_bytes: 1,
            nanos: 1,
            errors: 1,
        };
        telemetry.merge(&incoming);
        assert_eq!(
            telemetry.cost(HashSite::WholeRequest),
            HashCost {
                calls: u64::MAX,
                ram_bytes: u64::MAX,
                suffix_bytes: u64::MAX,
                metadata_bytes: u64::MAX,
                nanos: u64::MAX,
                errors: u64::MAX
            }
        );
        assert_eq!(telemetry.cost(HashSite::Checkpoint), HashCost::default());
        let counters = HostTelemetry {
            hashes: telemetry,
            ..HostTelemetry::default()
        }
        .counters();
        assert!(counters.contains(&("hash.whole_request.ram_bytes".to_owned(), u64::MAX)));
        assert!(counters.contains(&("hash.checkpoint.ram_bytes".to_owned(), 0)));
    }

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
