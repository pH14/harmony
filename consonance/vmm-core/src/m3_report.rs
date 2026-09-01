// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pure oracles for the M3 PostgreSQL liveness and performance report.
//!
//! The live HVF runner supplies serial bytes from the guest acceptance payload,
//! per-exit V-time from the normalized trace and guest pvclock page, and
//! phase-separated ARM wall-time/exit observations. This module performs no I/O
//! and reads no clock, so every pass/fail decision is unit-testable, including
//! the required planted failures. Optional x86 measurements are diagnostics,
//! never M3 acceptance inputs.

use std::collections::BTreeSet;

use thiserror::Error;

/// The fixed number of rows emitted by the acceptance workload.
pub const WORKLOAD_ROWS: u64 = 20;
/// The guest's `HZ=100` paravirtual tick period, in virtual nanoseconds.
pub const TICK_PERIOD_VNS: u64 = crate::vendor::arm64::contract::LINUX_CLOCKEVENT_PERIOD_VNS;
/// The documented maximum inter-exit gap is two tick periods.
pub const MAX_GAP_FACTOR: u64 = 2;
/// The largest permitted inter-exit gap, in virtual nanoseconds.
pub const MAX_GAP_VNS: u64 = TICK_PERIOD_VNS * MAX_GAP_FACTOR;
const CONTAINER_UP: &[u8] = b"DK38: container:";
const POSTGRES_START: &[u8] = b"PGC38: starting postgres in container";
const POSTGRES_READY: &[u8] = b"database system is ready to accept connections";
const WORKLOAD_END: &[u8] = b"PGC38: workload end";
const POSTGRES_STOPPED: &[u8] = b"PGC38: postgres stopped";
const DMESG_OK: &[u8] = b"M3_DMESG_OK";
const READY: &[u8] = b"ARM64_PG_M3_READY";

/// A failed M3 acceptance, gap, independent-comparator, or performance-evidence claim.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum M3ReportError {
    /// A required acceptance marker was absent from the serial stream.
    #[error("serial acceptance marker absent: {0}")]
    MissingMarker(&'static str),
    /// The guest explicitly reported a payload or kernel-health failure.
    #[error("guest reported failure marker: {0}")]
    FailureMarker(&'static str),
    /// The serial stream contains a kernel liveness failure.
    #[error("kernel liveness report present: {0}")]
    KernelLiveness(&'static str),
    /// A workload row did not match the fixed SQL oracle.
    #[error("malformed workload row {row}: {reason}")]
    MalformedRow {
        /// One-based row number expected at this position.
        row: u64,
        /// Concrete reason the line was rejected.
        reason: &'static str,
    },
    /// The serial stream carried the wrong number of workload rows.
    #[error("expected {expected} workload rows, observed {observed}")]
    WrongRowCount {
        /// Required row count.
        expected: u64,
        /// Parsed row count.
        observed: u64,
    },
    /// A UUID was repeated within the workload.
    #[error("workload UUID repeated at row {row}")]
    RepeatedUuid {
        /// One-based row containing the duplicate.
        row: u64,
    },
    /// V-time moved backward between consecutive observations.
    #[error("V-time regressed at observation {observation}: {before} -> {after}")]
    VtimeRegressed {
        /// Zero-based index of the later observation.
        observation: u64,
        /// Earlier V-time value.
        before: u64,
        /// Later V-time value.
        after: u64,
    },
    /// The trace contained too few values to measure an inter-exit gap.
    #[error("need at least two V-time observations, got {0}")]
    TooFewVtimeObservations(u64),
    /// The maximum gap exceeded the documented bound.
    #[error("maximum inter-exit gap {observed} vns exceeds {limit} vns")]
    GapTooLarge {
        /// Maximum observed gap.
        observed: u64,
        /// Permitted maximum gap.
        limit: u64,
    },
    /// The normalized trace and guest-visible pvclock comparator disagreed.
    #[error(
        "independent pvclock comparator disagreed: trace max/count {trace_max}/{trace_count}, \
         pvclock max/count {pvclock_max}/{pvclock_count}"
    )]
    ComparatorMismatch {
        /// Maximum derived from the normalized trace.
        trace_max: u64,
        /// Gap count derived from the normalized trace.
        trace_count: u64,
        /// Maximum observed independently through the guest pvclock page.
        pvclock_max: u64,
        /// Gap count observed independently through the guest pvclock page.
        pvclock_count: u64,
    },
    /// A phase did not advance both the event and host-diagnostic clocks.
    #[error(
        "ARM performance phase {phase} is empty or unordered: exits {start_exits}->{end_exits}, \
         wall_ns {start_wall_ns}->{end_wall_ns}"
    )]
    InvalidPerformancePhase {
        /// Stable report label for the rejected phase.
        phase: &'static str,
        /// Cumulative exit count at phase start.
        start_exits: u64,
        /// Cumulative exit count at phase end.
        end_exits: u64,
        /// Cumulative host wall nanoseconds at phase start.
        start_wall_ns: u64,
        /// Cumulative host wall nanoseconds at phase end.
        end_wall_ns: u64,
    },
    /// The event-loop count and independent normalized trace disagreed.
    #[error("ARM exit-count comparator disagreed: event loop {event_loop}, trace {trace}")]
    ExitCountMismatch {
        /// Exits counted by the host event loop.
        event_loop: u64,
        /// Events present in the normalized trace.
        trace: u64,
    },
    /// An optional descriptive-x86 diagnostic file was malformed.
    #[error("invalid optional descriptive-x86 diagnostic: {0}")]
    BaselineFormat(&'static str),
}

/// Successful parsing of the PostgreSQL acceptance stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptanceSummary {
    rows: u64,
    final_uuid: String,
    final_timestamp: String,
}

impl AcceptanceSummary {
    /// Number of SQL result rows parsed and checked.
    pub fn rows(&self) -> u64 {
        self.rows
    }

    /// UUID emitted by the final SQL row.
    pub fn final_uuid(&self) -> &str {
        &self.final_uuid
    }

    /// Timestamp emitted by the final SQL row.
    pub fn final_timestamp(&self) -> &str {
        &self.final_timestamp
    }
}

/// Fixed-bucket histogram and maximum for consecutive V-time observations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GapHistogram {
    counts: [u64; 8],
    observations: u64,
    max_gap_vns: u64,
}

impl GapHistogram {
    /// Build a histogram from ordered per-exit V-time values.
    ///
    /// Buckets are `0`, `1..=1us`, `1us..=100us`, `100us..=1ms`,
    /// `1ms..=5ms`, `5ms..=10ms`, `10ms..=20ms`, and `>20ms`.
    ///
    /// # Errors
    /// Rejects fewer than two samples or regressing V-time. Use
    /// [`Self::validate_bound`] for the milestone limit; keeping analysis and
    /// policy separate lets a failing live report retain the full histogram.
    pub fn analyze(values: &[u64]) -> Result<Self, M3ReportError> {
        let observations = u64::try_from(values.len()).unwrap_or(u64::MAX);
        if values.len() < 2 {
            return Err(M3ReportError::TooFewVtimeObservations(observations));
        }

        let mut counts = [0u64; 8];
        let mut max_gap_vns = 0u64;
        for (index, pair) in values.windows(2).enumerate() {
            let before = pair[0];
            let after = pair[1];
            let Some(gap) = after.checked_sub(before) else {
                return Err(M3ReportError::VtimeRegressed {
                    observation: u64::try_from(index + 1).unwrap_or(u64::MAX),
                    before,
                    after,
                });
            };
            max_gap_vns = max_gap_vns.max(gap);
            counts[bucket_index(gap)] += 1;
        }
        Ok(Self {
            counts,
            observations,
            max_gap_vns,
        })
    }

    /// Count of consecutive inter-exit gaps.
    pub fn gap_count(&self) -> u64 {
        self.observations - 1
    }

    /// Largest consecutive inter-exit gap, in virtual nanoseconds.
    pub fn max_gap_vns(&self) -> u64 {
        self.max_gap_vns
    }

    /// Fixed bucket counts in the order documented by [`Self::from_vns`].
    pub fn counts(&self) -> &[u64; 8] {
        &self.counts
    }

    /// Enforce the documented two-tick maximum.
    ///
    /// # Errors
    /// Returns [`M3ReportError::GapTooLarge`] when the measured maximum is
    /// greater than [`MAX_GAP_VNS`].
    pub fn validate_bound(&self) -> Result<(), M3ReportError> {
        if self.max_gap_vns > MAX_GAP_VNS {
            Err(M3ReportError::GapTooLarge {
                observed: self.max_gap_vns,
                limit: MAX_GAP_VNS,
            })
        } else {
            Ok(())
        }
    }
}

/// A whole-workload throughput sample using integer-only arithmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Throughput {
    /// Rows completed by the fixed workload.
    pub rows: u64,
    /// Host diagnostic duration in nanoseconds.
    pub wall_ns: u64,
}

/// Cumulative intrinsic ARM observation at a guest-authored phase marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PerformanceMark {
    /// Number of VMM exits completed when the marker was observed.
    pub exits: u64,
    /// Host diagnostic wall time elapsed since run start.
    pub wall_ns: u64,
}

/// One phase of intrinsic ARM performance evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhasePerformance {
    exits: u64,
    wall_ns: u64,
}

impl PhasePerformance {
    /// Derive one phase from cumulative start/end observations.
    ///
    /// # Errors
    /// Rejects a phase that did not advance both exit count and diagnostic wall
    /// time. This makes missing/coalesced phase evidence fail loudly.
    pub fn between(
        phase: &'static str,
        start: PerformanceMark,
        end: PerformanceMark,
    ) -> Result<Self, M3ReportError> {
        let Some(exits) = end.exits.checked_sub(start.exits) else {
            return Err(M3ReportError::InvalidPerformancePhase {
                phase,
                start_exits: start.exits,
                end_exits: end.exits,
                start_wall_ns: start.wall_ns,
                end_wall_ns: end.wall_ns,
            });
        };
        let Some(wall_ns) = end.wall_ns.checked_sub(start.wall_ns) else {
            return Err(M3ReportError::InvalidPerformancePhase {
                phase,
                start_exits: start.exits,
                end_exits: end.exits,
                start_wall_ns: start.wall_ns,
                end_wall_ns: end.wall_ns,
            });
        };
        if exits == 0 || wall_ns == 0 {
            return Err(M3ReportError::InvalidPerformancePhase {
                phase,
                start_exits: start.exits,
                end_exits: end.exits,
                start_wall_ns: start.wall_ns,
                end_wall_ns: end.wall_ns,
            });
        }
        Ok(Self { exits, wall_ns })
    }

    /// VMM exits completed during this phase.
    pub fn exits(self) -> u64 {
        self.exits
    }

    /// Host diagnostic duration of this phase.
    pub fn wall_ns(self) -> u64 {
        self.wall_ns
    }

    /// Integer milli-exits per second, saturated on arithmetic overflow.
    pub fn milli_exits_per_second(self) -> u128 {
        u128::from(self.exits).saturating_mul(1_000_000_000_000) / u128::from(self.wall_ns)
    }
}

impl Throughput {
    /// Integer milli-rows per second, saturated on arithmetic overflow.
    pub fn milli_rows_per_second(self) -> u128 {
        u128::from(self.rows).saturating_mul(1_000_000_000_000) / u128::from(self.wall_ns.max(1))
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn find_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

fn is_uuid(value: &[u8]) -> bool {
    value.len() == 36
        && value.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn is_timestamp(value: &[u8]) -> bool {
    value.len() >= 19
        && value[0..4].iter().all(u8::is_ascii_digit)
        && value[4] == b'-'
        && value[5..7].iter().all(u8::is_ascii_digit)
        && value[7] == b'-'
        && value[8..10].iter().all(u8::is_ascii_digit)
        && value[10] == b' '
        && value[11..13].iter().all(u8::is_ascii_digit)
        && value[13] == b':'
        && value[14..16].iter().all(u8::is_ascii_digit)
        && value[16] == b':'
        && value[17..19].iter().all(u8::is_ascii_digit)
}

fn bucket_index(gap: u64) -> usize {
    match gap {
        0 => 0,
        1..=1_000 => 1,
        1_001..=100_000 => 2,
        100_001..=1_000_000 => 3,
        1_000_001..=5_000_000 => 4,
        5_000_001..=10_000_000 => 5,
        10_000_001..=MAX_GAP_VNS => 6,
        _ => 7,
    }
}

fn require_marker(
    serial: &[u8],
    marker: &'static [u8],
    name: &'static str,
) -> Result<(), M3ReportError> {
    if find(serial, marker) {
        Ok(())
    } else {
        Err(M3ReportError::MissingMarker(name))
    }
}

/// Validate the complete guest acceptance stream and return its checked row evidence.
///
/// # Errors
/// Rejects missing lifecycle markers, guest failure markers, kernel liveness
/// reports, malformed SQL aggregates, malformed UUID/timestamp fields, repeated
/// UUIDs, and any row count other than [`WORKLOAD_ROWS`].
pub fn validate_acceptance(serial: &[u8]) -> Result<AcceptanceSummary, M3ReportError> {
    for (marker, name) in [
        (b"M3_POSTGRES_FAIL".as_slice(), "M3_POSTGRES_FAIL"),
        (b"M3_KERNEL_HEALTH_FAIL".as_slice(), "M3_KERNEL_HEALTH_FAIL"),
    ] {
        if find(serial, marker) {
            return Err(M3ReportError::FailureMarker(name));
        }
    }
    for (needle, name) in [
        (b"rcu stall".as_slice(), "RCU stall"),
        (b"soft lockup".as_slice(), "soft lockup"),
        (b"watchdog: BUG".as_slice(), "watchdog BUG"),
    ] {
        if find_ascii_case_insensitive(serial, needle) {
            return Err(M3ReportError::KernelLiveness(name));
        }
    }
    for (marker, name) in [
        (CONTAINER_UP, "container isolation"),
        (POSTGRES_START, "postgres start"),
        (POSTGRES_READY, "postgres ready"),
        (WORKLOAD_END, "workload end"),
        (POSTGRES_STOPPED, "postgres stopped"),
        (DMESG_OK, "guest dmesg oracle"),
        (READY, "M3 terminal ready"),
    ] {
        require_marker(serial, marker, name)?;
    }

    let mut rows = 0u64;
    let mut uuids = BTreeSet::new();
    let mut final_uuid = String::new();
    let mut final_timestamp = String::new();
    for raw_line in serial.split(|byte| *byte == b'\n') {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if !line.starts_with(b"row|") {
            continue;
        }
        rows += 1;
        if rows > WORKLOAD_ROWS {
            continue;
        }
        let fields: Vec<&[u8]> = line.split(|byte| *byte == b'|').collect();
        if fields.len() != 6 {
            return Err(M3ReportError::MalformedRow {
                row: rows,
                reason: "expected six pipe-delimited fields",
            });
        }
        let expected_i = rows.to_string();
        let expected_sum = (rows * (rows + 1) / 2).to_string();
        if fields[1] != expected_i.as_bytes()
            || fields[2] != expected_i.as_bytes()
            || fields[3] != expected_sum.as_bytes()
        {
            return Err(M3ReportError::MalformedRow {
                row: rows,
                reason: "index/count/sum does not match the fixed SQL oracle",
            });
        }
        if !is_uuid(fields[4]) {
            return Err(M3ReportError::MalformedRow {
                row: rows,
                reason: "UUID field has the wrong shape",
            });
        }
        if !is_timestamp(fields[5]) {
            return Err(M3ReportError::MalformedRow {
                row: rows,
                reason: "timestamp field has the wrong shape",
            });
        }
        if !uuids.insert(fields[4].to_vec()) {
            return Err(M3ReportError::RepeatedUuid { row: rows });
        }
        if rows == WORKLOAD_ROWS {
            final_uuid = String::from_utf8_lossy(fields[4]).into_owned();
            final_timestamp = String::from_utf8_lossy(fields[5]).into_owned();
        }
    }
    if rows != WORKLOAD_ROWS {
        return Err(M3ReportError::WrongRowCount {
            expected: WORKLOAD_ROWS,
            observed: rows,
        });
    }
    Ok(AcceptanceSummary {
        rows,
        final_uuid,
        final_timestamp,
    })
}

/// Require the normalized-trace histogram to match the independent pvclock stream.
///
/// # Errors
/// Returns [`M3ReportError::ComparatorMismatch`] if either maximum or gap count differs.
pub fn compare_gap_oracles(
    trace: &GapHistogram,
    pvclock_max: u64,
    pvclock_count: u64,
) -> Result<(), M3ReportError> {
    if (trace.max_gap_vns(), trace.gap_count()) == (pvclock_max, pvclock_count) {
        Ok(())
    } else {
        Err(M3ReportError::ComparatorMismatch {
            trace_max: trace.max_gap_vns(),
            trace_count: trace.gap_count(),
            pvclock_max,
            pvclock_count,
        })
    }
}

/// Compare the intrinsic event-loop exit count with the backend-local raw trace.
///
/// # Errors
/// Rejects any disagreement, including a trace truncated by one event.
pub fn compare_exit_counts(event_loop: u64, trace: u64) -> Result<(), M3ReportError> {
    if event_loop == trace {
        Ok(())
    } else {
        Err(M3ReportError::ExitCountMismatch { event_loop, trace })
    }
}

/// Parse the exact optional diagnostic file emitted by descriptive x86.
///
/// The five-line format binds the payload and mode before supplying the integer
/// row count and wall duration. The caller may print this beside the intrinsic
/// ARM evidence, but it must not affect M3 status.
///
/// # Errors
/// Rejects non-UTF-8, missing/extra lines, a different payload or execution
/// mode, non-integer fields, and a row count other than [`WORKLOAD_ROWS`].
pub fn parse_x86_diagnostic(bytes: &[u8]) -> Result<Throughput, M3ReportError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| M3ReportError::BaselineFormat("file is not UTF-8"))?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() != 5 {
        return Err(M3ReportError::BaselineFormat("expected exactly five lines"));
    }
    if lines[0] != "format consonance.m3-x86-diagnostic.v1" {
        return Err(M3ReportError::BaselineFormat("wrong format identifier"));
    }
    if lines[1] != "payload postgres-container-task38" {
        return Err(M3ReportError::BaselineFormat("wrong payload identifier"));
    }
    if lines[2] != "mode descriptive-x86" {
        return Err(M3ReportError::BaselineFormat("wrong execution mode"));
    }
    let rows = lines[3]
        .strip_prefix("rows ")
        .ok_or(M3ReportError::BaselineFormat("missing rows field"))?
        .parse::<u64>()
        .map_err(|_| M3ReportError::BaselineFormat("rows is not a u64"))?;
    let wall_ns = lines[4]
        .strip_prefix("wall_ns ")
        .ok_or(M3ReportError::BaselineFormat("missing wall_ns field"))?
        .parse::<u64>()
        .map_err(|_| M3ReportError::BaselineFormat("wall_ns is not a u64"))?;
    if rows != WORKLOAD_ROWS {
        return Err(M3ReportError::BaselineFormat(
            "row count does not match the fixed workload",
        ));
    }
    if wall_ns == 0 {
        return Err(M3ReportError::BaselineFormat("wall_ns must be nonzero"));
    }
    Ok(Throughput { rows, wall_ns })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serial() -> Vec<u8> {
        let mut out = b"DK38: container: namespaces\nPGC38: starting postgres in container\n\
            database system is ready to accept connections\n"
            .to_vec();
        for row in 1..=WORKLOAD_ROWS {
            let sum = row * (row + 1) / 2;
            out.extend_from_slice(
                format!(
                    "row|{row}|{row}|{sum}|00000000-0000-4000-8000-{row:012}|\
                     2026-08-26 12:34:{row:02}+00\n"
                )
                .as_bytes(),
            );
        }
        out.extend_from_slice(
            b"PGC38: workload end\nPGC38: postgres stopped\nM3_DMESG_OK\nARM64_PG_M3_READY\n",
        );
        out
    }

    #[test]
    fn acceptance_positive_checks_all_rows() {
        let summary = validate_acceptance(&serial()).unwrap();
        assert_eq!(summary.rows(), WORKLOAD_ROWS);
        assert_eq!(summary.final_uuid(), "00000000-0000-4000-8000-000000000020");
        assert_eq!(summary.final_timestamp(), "2026-08-26 12:34:20+00");
    }

    #[test]
    fn acceptance_negative_rejects_missing_real_completion() {
        let mut malformed = serial();
        let end = malformed.len() - b"ARM64_PG_M3_READY\n".len();
        malformed.truncate(end);
        assert_eq!(
            validate_acceptance(&malformed),
            Err(M3ReportError::MissingMarker("M3 terminal ready"))
        );
    }

    #[test]
    fn acceptance_negative_rejects_false_sql_aggregate() {
        let malformed = String::from_utf8(serial())
            .unwrap()
            .replace("row|20|20|210|", "row|20|20|209|")
            .into_bytes();
        assert!(matches!(
            validate_acceptance(&malformed),
            Err(M3ReportError::MalformedRow { row: 20, .. })
        ));
    }

    #[test]
    fn uuid_timestamp_and_gap_buckets_reject_every_single_field_corruption() {
        let uuid = b"00000000-0000-4000-8000-000000000020";
        assert!(is_uuid(uuid));
        assert!(!is_uuid(&uuid[..35]));
        assert!(!is_uuid(&[uuid.as_slice(), b"0"].concat()));
        for index in 0..uuid.len() {
            let mut bad = uuid.to_vec();
            bad[index] = if matches!(index, 8 | 13 | 18 | 23) {
                b'0'
            } else {
                b'g'
            };
            assert!(!is_uuid(&bad), "UUID byte {index} must be validated");
        }

        let timestamp = b"2026-08-26 12:34:20";
        assert!(is_timestamp(timestamp));
        assert!(!is_timestamp(&timestamp[..18]));
        for index in 0..timestamp.len() {
            let mut bad = timestamp.to_vec();
            bad[index] = match index {
                4 | 7 => b'/',
                10 => b'T',
                13 | 16 => b'.',
                _ => b'x',
            };
            assert!(
                !is_timestamp(&bad),
                "timestamp byte {index} must be validated"
            );
        }

        for (gap, expected) in [
            (0, 0),
            (1, 1),
            (1_000, 1),
            (1_001, 2),
            (100_000, 2),
            (100_001, 3),
            (1_000_000, 3),
            (1_000_001, 4),
            (5_000_000, 4),
            (5_000_001, 5),
            (10_000_000, 5),
            (10_000_001, 6),
            (MAX_GAP_VNS, 6),
            (MAX_GAP_VNS + 1, 7),
        ] {
            assert_eq!(bucket_index(gap), expected, "gap {gap}");
        }
    }

    #[test]
    fn acceptance_rejects_each_sql_identity_field_and_extra_rows() {
        for replacement in ["row|20|19|210|", "row|20|20|209|"] {
            let malformed = String::from_utf8(serial())
                .unwrap()
                .replace("row|20|20|210|", replacement)
                .into_bytes();
            assert!(matches!(
                validate_acceptance(&malformed),
                Err(M3ReportError::MalformedRow { row: 20, .. })
            ));
        }

        let mut extra = serial();
        let marker = b"PGC38: workload end\n";
        let position = extra
            .windows(marker.len())
            .position(|w| w == marker)
            .unwrap();
        extra.splice(
            position..position,
            b"row|21|21|231|00000000-0000-4000-8000-000000000021|2026-08-26 12:34:21+00\n"
                .iter()
                .copied(),
        );
        assert_eq!(
            validate_acceptance(&extra),
            Err(M3ReportError::WrongRowCount {
                expected: WORKLOAD_ROWS,
                observed: WORKLOAD_ROWS + 1,
            })
        );

        // Rows beyond the fixed workload are counted but deliberately not
        // parsed: their content is outside the SQL oracle. This pins the
        // boundary independently from the final row-count rejection.
        let mut malformed_extra = serial();
        let position = malformed_extra
            .windows(marker.len())
            .position(|w| w == marker)
            .unwrap();
        malformed_extra.splice(position..position, b"row|malformed\n".iter().copied());
        assert_eq!(
            validate_acceptance(&malformed_extra),
            Err(M3ReportError::WrongRowCount {
                expected: WORKLOAD_ROWS,
                observed: WORKLOAD_ROWS + 1,
            })
        );
    }

    #[test]
    fn gap_positive_and_independent_comparator_agree() {
        let histogram = GapHistogram::analyze(&[10, 10, 1_010, 10_001_010]).unwrap();
        histogram.validate_bound().unwrap();
        assert_eq!(MAX_GAP_VNS, 20_000_000);
        assert_eq!(histogram.max_gap_vns(), 10_000_000);
        assert_eq!(histogram.gap_count(), 3);
        assert_eq!(histogram.counts(), &[1, 1, 0, 0, 0, 1, 0, 0]);
        compare_gap_oracles(&histogram, 10_000_000, 3).unwrap();
    }

    #[test]
    fn gap_regression_index_and_exact_bound_are_pinned() {
        assert_eq!(
            GapHistogram::analyze(&[0, 10, 5]),
            Err(M3ReportError::VtimeRegressed {
                observation: 2,
                before: 10,
                after: 5,
            })
        );
        GapHistogram::analyze(&[0, MAX_GAP_VNS])
            .unwrap()
            .validate_bound()
            .unwrap();
    }

    #[test]
    fn gap_negative_rejects_more_than_two_tick_periods() {
        assert_eq!(
            GapHistogram::analyze(&[0, MAX_GAP_VNS + 1])
                .unwrap()
                .validate_bound(),
            Err(M3ReportError::GapTooLarge {
                observed: MAX_GAP_VNS + 1,
                limit: MAX_GAP_VNS,
            })
        );
    }

    #[test]
    fn independent_comparator_negative_rejects_mismatch() {
        let histogram = GapHistogram::analyze(&[0, 10, 20]).unwrap();
        assert!(matches!(
            compare_gap_oracles(&histogram, 11, 2),
            Err(M3ReportError::ComparatorMismatch { .. })
        ));
    }

    #[test]
    fn intrinsic_performance_positive_reports_phase_density() {
        let phase = PhasePerformance::between(
            "workload",
            PerformanceMark {
                exits: 1_000,
                wall_ns: 2_000,
            },
            PerformanceMark {
                exits: 1_100,
                wall_ns: 1_002_000,
            },
        )
        .unwrap();
        assert_eq!(phase.exits(), 100);
        assert_eq!(phase.wall_ns(), 1_000_000);
        assert_eq!(phase.milli_exits_per_second(), 100_000_000);
    }

    #[test]
    fn intrinsic_performance_negative_rejects_empty_phase() {
        let mark = PerformanceMark {
            exits: 1_000,
            wall_ns: 2_000,
        };
        assert!(matches!(
            PhasePerformance::between("workload", mark, mark),
            Err(M3ReportError::InvalidPerformancePhase {
                phase: "workload",
                ..
            })
        ));
        assert!(matches!(
            PhasePerformance::between(
                "no exits",
                mark,
                PerformanceMark {
                    exits: mark.exits,
                    wall_ns: mark.wall_ns + 1,
                },
            ),
            Err(M3ReportError::InvalidPerformancePhase {
                phase: "no exits",
                ..
            })
        ));
        assert!(matches!(
            PhasePerformance::between(
                "no wall time",
                mark,
                PerformanceMark {
                    exits: mark.exits + 1,
                    wall_ns: mark.wall_ns,
                },
            ),
            Err(M3ReportError::InvalidPerformancePhase {
                phase: "no wall time",
                ..
            })
        ));
    }

    #[test]
    fn throughput_integer_scale_and_zero_duration_are_exact() {
        assert_eq!(
            Throughput {
                rows: 20,
                wall_ns: 2_000_000_000,
            }
            .milli_rows_per_second(),
            10_000
        );
        assert_eq!(
            Throughput {
                rows: 2,
                wall_ns: 0,
            }
            .milli_rows_per_second(),
            2_000_000_000_000
        );
    }

    #[test]
    fn kernel_liveness_markers_are_case_insensitive() {
        let mut failed = serial();
        failed.extend_from_slice(b"RcU StAlL detected\n");
        assert_eq!(
            validate_acceptance(&failed),
            Err(M3ReportError::KernelLiveness("RCU stall"))
        );
    }

    #[test]
    fn exit_count_independent_comparator_positive_and_negative() {
        compare_exit_counts(101_792, 101_792).unwrap();
        assert_eq!(
            compare_exit_counts(101_792, 101_791),
            Err(M3ReportError::ExitCountMismatch {
                event_loop: 101_792,
                trace: 101_791,
            })
        );
    }

    #[test]
    fn optional_x86_diagnostic_parser_binds_payload_and_mode() {
        let sample = parse_x86_diagnostic(
            b"format consonance.m3-x86-diagnostic.v1\n\
              payload postgres-container-task38\n\
              mode descriptive-x86\n\
              rows 20\n\
              wall_ns 123456\n",
        )
        .unwrap();
        assert_eq!(sample.rows, WORKLOAD_ROWS);
        assert_eq!(sample.wall_ns, 123_456);
    }

    #[test]
    fn optional_x86_diagnostic_negative_rejects_wrong_mode() {
        assert_eq!(
            parse_x86_diagnostic(
                b"format consonance.m3-x86-diagnostic.v1\n\
                  payload postgres-container-task38\n\
                  mode virtual_time-arm64\n\
                  rows 20\n\
                  wall_ns 123456\n",
            ),
            Err(M3ReportError::BaselineFormat("wrong execution mode"))
        );
    }
}
