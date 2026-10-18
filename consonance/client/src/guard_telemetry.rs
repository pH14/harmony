// SPDX-License-Identifier: AGPL-3.0-or-later

use host_code_identity::GuardMeasurement;
use std::sync::Mutex;

#[derive(Clone, Copy, Debug)]
pub(crate) enum GuardSite {
    Admission,
    SessionBoundary,
    ParentRpcBefore,
    ParentRpcAfter,
    LauncherBefore,
    LauncherAfter,
    ChildReceive,
    ChildHandled,
    ChildSent,
    PublicationBefore,
    PublicationAfter,
    ControlBefore,
    ControlAfter,
    SdkRequest,
    SdkResponse,
}

impl GuardSite {
    const ALL: [Self; 15] = [
        Self::Admission,
        Self::SessionBoundary,
        Self::ParentRpcBefore,
        Self::ParentRpcAfter,
        Self::LauncherBefore,
        Self::LauncherAfter,
        Self::ChildReceive,
        Self::ChildHandled,
        Self::ChildSent,
        Self::PublicationBefore,
        Self::PublicationAfter,
        Self::ControlBefore,
        Self::ControlAfter,
        Self::SdkRequest,
        Self::SdkResponse,
    ];
    fn name(self) -> &'static str {
        match self {
            Self::Admission => "admission",
            Self::SessionBoundary => "session_boundary",
            Self::ParentRpcBefore => "rpc_before",
            Self::ParentRpcAfter => "rpc_after",
            Self::LauncherBefore => "launcher_before",
            Self::LauncherAfter => "launcher_after",
            Self::ChildReceive => "receive",
            Self::ChildHandled => "handled",
            Self::ChildSent => "sent",
            Self::PublicationBefore => "publication_before",
            Self::PublicationAfter => "publication_after",
            Self::ControlBefore => "control_before",
            Self::ControlAfter => "control_after",
            Self::SdkRequest => "sdk_request",
            Self::SdkResponse => "sdk_response",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum GuardDispatch {
    Capture,
    Check,
}

#[derive(Clone, Copy, Debug, Default)]
struct DispatchWork {
    attempts: u64,
    errors: u64,
    elapsed_ns: u64,
}

impl DispatchWork {
    fn merge(&mut self, work: GuardMeasurement) {
        self.attempts = self.attempts.saturating_add(work.attempts);
        self.errors = self.errors.saturating_add(work.errors);
        self.elapsed_ns = self.elapsed_ns.saturating_add(work.elapsed_ns);
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct SiteWork {
    dispatches: u64,
    errors: u64,
    elapsed_ns: u64,
    fixture_dispatches: u64,
    strict: GuardMeasurement,
    checks: DispatchWork,
    captures: DispatchWork,
}

#[derive(Debug)]
pub(crate) struct GuardTelemetry(Mutex<[SiteWork; 15]>);

impl Default for GuardTelemetry {
    fn default() -> Self {
        Self(Mutex::new(std::array::from_fn(|_| SiteWork::default())))
    }
}

impl GuardTelemetry {
    pub(crate) fn record(
        &self,
        site: GuardSite,
        dispatch: GuardDispatch,
        strict: GuardMeasurement,
        error: bool,
        fixture: bool,
        elapsed_ns: u64,
    ) {
        let mut sites = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let work = &mut sites[site as usize];
        work.dispatches = work.dispatches.saturating_add(1);
        work.errors = work.errors.saturating_add(u64::from(error));
        work.fixture_dispatches = work.fixture_dispatches.saturating_add(u64::from(fixture));
        work.elapsed_ns = work.elapsed_ns.saturating_add(elapsed_ns);
        match dispatch {
            GuardDispatch::Capture => work.captures.merge(strict),
            GuardDispatch::Check => work.checks.merge(strict),
        }
        work.strict.merge(strict);
    }

    pub(crate) fn counters(&self, prefix: &str) -> Vec<(String, u64)> {
        let sites = *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut out = Vec::new();
        for site in GuardSite::ALL {
            let work = sites[site as usize];
            let key = format!("{prefix}.{}", site.name());
            let strict = work.strict;
            for (name, value) in [
                ("dispatches", work.dispatches),
                ("errors", work.errors),
                ("ns", work.elapsed_ns),
                ("fixture_dispatches", work.fixture_dispatches),
                ("strict_attempts", strict.attempts),
                ("strict_errors", strict.errors),
                ("strict_ns", strict.elapsed_ns),
                ("strict_check_attempts", work.checks.attempts),
                ("strict_check_errors", work.checks.errors),
                ("strict_check_ns", work.checks.elapsed_ns),
                ("strict_capture_attempts", work.captures.attempts),
                ("strict_capture_errors", work.captures.errors),
                ("strict_capture_ns", work.captures.elapsed_ns),
                ("kernel_state_checks", strict.kernel_state_checks),
                ("static_signing_checks", strict.static_signing_checks),
                ("image_censuses", strict.image_censuses),
                ("bytes_available", u64::from(strict.bytes.is_some())),
            ] {
                out.push((format!("{key}.{name}"), value));
            }
            if let Some(bytes) = strict.bytes {
                for (name, value) in [
                    ("live_exec_read_bytes", bytes.live_exec_read_bytes),
                    ("sealed_exec_read_bytes", bytes.sealed_exec_read_bytes),
                    ("live_exec_hashed_bytes", bytes.live_exec_hashed_bytes),
                    ("comparison_input_bytes", bytes.comparison_input_bytes),
                    ("sealed_file_read_bytes", bytes.sealed_file_read_bytes),
                    ("sealed_file_hashed_bytes", bytes.sealed_file_hashed_bytes),
                ] {
                    out.push((format!("{key}.{name}"), value));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_and_check_work_remain_separate_disjoint_stages() {
        let telemetry = GuardTelemetry::default();
        let check = GuardMeasurement {
            attempts: 1,
            errors: 1,
            elapsed_ns: 7,
            ..GuardMeasurement::default()
        };
        let capture = GuardMeasurement {
            attempts: 1,
            errors: 0,
            elapsed_ns: 11,
            ..GuardMeasurement::default()
        };
        telemetry.record(
            GuardSite::Admission,
            GuardDispatch::Check,
            check,
            true,
            false,
            9,
        );
        telemetry.record(
            GuardSite::Admission,
            GuardDispatch::Capture,
            capture,
            false,
            false,
            13,
        );
        let counters = telemetry
            .counters("test")
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(counters["test.admission.dispatches"], 2);
        assert_eq!(counters["test.admission.strict_attempts"], 2);
        assert_eq!(counters["test.admission.strict_check_attempts"], 1);
        assert_eq!(counters["test.admission.strict_capture_attempts"], 1);
        assert_eq!(counters["test.admission.strict_check_errors"], 1);
        assert_eq!(counters["test.admission.strict_capture_errors"], 0);
        assert_eq!(counters["test.admission.strict_check_ns"], 7);
        assert_eq!(counters["test.admission.strict_capture_ns"], 11);
        assert_eq!(counters["test.admission.strict_ns"], 18);
    }
}
