// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stage 1 — the Linux half: measuring the counter.
//!
//! Exactness comes from a differential: two counted windows at two iteration
//! counts, judged against the payload's own branch analysis. Overflow delivery
//! and skid come from the sampling ring, one arm at a time, so multiplicity is
//! proven per arm rather than inferred from a tally.
//!
//! Every arm and every repetition is retained. The report recomputes the floors
//! from those records; nothing here writes a verdict.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use crate::payload::{self, PayloadSpec};
use crate::perf::Scope;
use crate::perf_sys::{PerfCounter, PerfError, pin_to_core};
use crate::report::Record;
use crate::stage1::{
    Interference, MeasurementPlan, SkidBuckets, Stage1Error, Stage1Outcome, interrupts_for_core,
    iterations_for, oracle_delta, projection,
};

/// How much memory the memory-pressure thread churns. Larger than any
/// last-level cache the known-chip table covers, so the payload's working set
/// is actually evicted rather than sharing a warm cache.
const PRESSURE_BYTES: usize = 256 * 1024 * 1024;

impl From<PerfError> for Stage1Error {
    fn from(error: PerfError) -> Stage1Error {
        Stage1Error::Counter {
            what: "a work-clock counter operation".to_string(),
            detail: error.to_string(),
        }
    }
}

fn read_file(what: &str, path: &str) -> Result<String, Stage1Error> {
    std::fs::read_to_string(path).map_err(|e| Stage1Error::Read {
        what: format!("{what} ({path})"),
        detail: e.to_string(),
    })
}

/// How many interrupts have been delivered to `core` so far.
///
/// # Errors
/// [`Stage1Error::Read`] when `/proc/interrupts` cannot be read.
pub fn interrupts_on_core(core: usize) -> Result<u64, Stage1Error> {
    let text = read_file("the interrupt counters", "/proc/interrupts")?;
    Ok(interrupts_for_core(&text, core))
}

/// The measurement core's simultaneous-multithreading sibling, when it has one.
///
/// # Errors
/// [`Stage1Error::Read`] when the topology cannot be read.
pub fn smt_sibling(core: usize) -> Result<Option<usize>, Stage1Error> {
    let path = format!("/sys/devices/system/cpu/cpu{core}/topology/thread_siblings_list");
    let text = read_file("the thread-sibling list", &path)?;
    Ok(crate::stage0::parse_cpu_list(&text)
        .into_iter()
        .find(|c| *c != core))
}

/// A core other than the measurement core and its sibling.
fn other_core(core: usize, sibling: Option<usize>) -> Result<usize, Stage1Error> {
    let text = read_file("the online CPU list", "/sys/devices/system/cpu/online")?;
    crate::stage0::parse_cpu_list(&text)
        .into_iter()
        .find(|c| *c != core && Some(*c) != sibling)
        .ok_or_else(|| Stage1Error::Unavailable {
            what: "a co-tenant core".to_string(),
            detail: format!(
                "no online CPU other than the measurement core {core} and its sibling \
                 {sibling:?}"
            ),
        })
}

/// A background load held for as long as the guard lives.
struct Load {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Load {
    /// No load at all.
    fn none() -> Load {
        Load {
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
        }
    }

    /// A thread pinned to `core`, spinning until the guard drops.
    fn spinning_on(core: usize) -> Load {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            // A thread that cannot pin still spins: it is then a co-tenant on
            // some core rather than none, which the run records as the
            // condition it asked for only because the caller checked the pin.
            let _ = pin_to_core(core);
            let mut acc = 0u64;
            while !flag.load(Ordering::Relaxed) {
                acc = std::hint::black_box(
                    acc.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1),
                );
            }
        });
        Load {
            stop,
            thread: Some(thread),
        }
    }

    /// A thread walking a buffer larger than any cache, until the guard drops.
    fn churning_memory() -> Load {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut buffer = vec![0u8; PRESSURE_BYTES];
            let mut cursor = 0usize;
            while !flag.load(Ordering::Relaxed) {
                // A cache-line stride touches every line without the prefetcher
                // turning the walk into one streaming read.
                cursor = (cursor + 64) % buffer.len();
                buffer[cursor] = buffer[cursor].wrapping_add(1);
                std::hint::black_box(&buffer[cursor]);
            }
        });
        Load {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Load {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Start the load a condition names.
fn start_load(condition: Interference, plan: &MeasurementPlan) -> Result<Load, Stage1Error> {
    match condition {
        Interference::Quiet => Ok(Load::none()),
        Interference::SmtSibling => {
            let sibling = smt_sibling(plan.core)?.ok_or_else(|| Stage1Error::Unavailable {
                what: "the simultaneous-multithreading sibling probe".to_string(),
                detail: format!("core {} has no sibling on this host", plan.core),
            })?;
            Ok(Load::spinning_on(sibling))
        }
        Interference::CoTenant => {
            let sibling = smt_sibling(plan.core)?;
            Ok(Load::spinning_on(other_core(plan.core, sibling)?))
        }
        Interference::MemoryPressure => Ok(Load::churning_memory()),
    }
}

/// One counted window: reset, enable, run `n` iterations, disable, read.
fn counted_window(
    counter: &PerfCounter,
    spec: &PayloadSpec,
    n: u64,
    core: usize,
) -> Result<(u64, bool, u64), Stage1Error> {
    let before = interrupts_on_core(core)?;
    counter.reset()?;
    counter.enable()?;
    let ran = payload::run(spec, n);
    counter.disable()?;
    let after = interrupts_on_core(core)?;
    if ran.is_none() {
        return Err(Stage1Error::Unavailable {
            what: format!("payload {}", spec.name),
            detail: "this architecture has no body for it".to_string(),
        });
    }
    let read = counter.read_timed()?;
    Ok((read.value, read.multiplexed(), after.saturating_sub(before)))
}

/// Measure count exactness for one payload under one condition.
///
/// # Errors
/// [`Stage1Error::BadScales`] when the differential is degenerate, and any
/// counter or read refusal.
pub fn measure_exactness(
    config: u64,
    spec: &PayloadSpec,
    plan: &MeasurementPlan,
    condition: Interference,
) -> Result<Vec<Record>, Stage1Error> {
    if plan.n1 >= plan.n2 {
        return Err(Stage1Error::BadScales {
            payload: spec.name.to_string(),
            n1: plan.n1,
            n2: plan.n2,
        });
    }
    let counter = PerfCounter::open_counting(config, Scope::HostUser)?;
    let _load = start_load(condition, plan)?;
    let mut records = Vec::new();
    for rep in 0..plan.reps {
        let (count_n1, mux_n1, irqs_n1) = counted_window(&counter, spec, plan.n1, plan.core)?;
        let (count_n2, mux_n2, irqs_n2) = counted_window(&counter, spec, plan.n2, plan.core)?;
        records.push(Record::Exactness {
            payload: spec.name.to_string(),
            condition: condition.token().to_string(),
            rep,
            n1: plan.n1,
            n2: plan.n2,
            count_n1,
            count_n2,
            oracle_delta: oracle_delta(spec, plan.n1, plan.n2),
            events_per_iteration: spec.events_per_iteration,
            multiplexed: mux_n1 || mux_n2,
            irqs_n1,
            irqs_n2,
        });
    }
    Ok(records)
}

/// Arm one overflow at `period`, run a payload that must cross it, and read the
/// ring.
fn one_arm(
    counter: &PerfCounter,
    spec: &PayloadSpec,
    n: u64,
    period: u64,
) -> Result<(u64, u64, u64, u64), Stage1Error> {
    counter.reset()?;
    // Re-setting the period before each arm restarts the countdown, so the
    // overflow fires after exactly `period` rather than after whatever was left
    // from the previous arm.
    counter.set_period(period)?;
    counter.refresh(1)?;
    let ran = payload::run(spec, n);
    counter.disable()?;
    if ran.is_none() {
        return Err(Stage1Error::Unavailable {
            what: format!("payload {}", spec.name),
            detail: "this architecture has no body for it".to_string(),
        });
    }
    let scan = counter.scan_ring();
    Ok((scan.samples, scan.last_value, scan.lost, scan.throttle))
}

/// Measure overflow delivery and skid for one payload at one period.
///
/// # Errors
/// Any counter refusal, and [`Stage1Error::Unavailable`] when the payload has no
/// body on this architecture.
pub fn measure_overflow(
    config: u64,
    spec: &PayloadSpec,
    period: u64,
    arms: u64,
    first_idx: u64,
) -> Result<(Vec<Record>, SkidBuckets), Stage1Error> {
    let counter = PerfCounter::open_sampling(config, Scope::HostUser, period)?;
    let n = iterations_for(spec, period);
    let mut records = Vec::with_capacity(usize::try_from(arms).unwrap_or(0));
    let mut buckets = SkidBuckets::default();
    let mut delivered = 0u64;
    let mut lost = 0u64;
    let mut duplicated = 0u64;
    for offset in 0..arms {
        let (samples, value, dropped, throttled) = one_arm(&counter, spec, n, period)?;
        if dropped == 0 && throttled == 0 {
            match samples {
                0 => lost += 1,
                1 if value >= period => {
                    delivered += 1;
                    buckets.observe(value - period);
                }
                1 => {}
                _ => duplicated += 1,
            }
        }
        records.push(Record::OverflowArm {
            payload: spec.name.to_string(),
            idx: first_idx + offset,
            period,
            samples,
            value_at_interrupt: value,
            dropped,
            throttled,
        });
    }
    // The run's own tally, written beside the per-arm records the report
    // recomputes from. It is cross-checked there, never believed.
    records.push(Record::OverflowSummary {
        payload: spec.name.to_string(),
        arms_total: arms,
        delivered_once: delivered,
        lost,
        duplicated,
        skid_max: buckets.max,
    });
    Ok((records, buckets))
}

/// Run stage 1 on this host.
///
/// The measurement thread is pinned first: an unpinned counter measures
/// whichever core the scheduler chose, which is not a measurement.
///
/// # Errors
/// Any counter, read, or availability refusal.
pub fn run(config: u64, plan: &MeasurementPlan) -> Result<Stage1Outcome, Stage1Error> {
    pin_to_core(plan.core)?;
    let mut records = Vec::new();

    // The discipline: measure a short slice, project the campaign from it, and
    // retain the projection before the campaign starts.
    let slice = plan.slice(1, 32);
    let specs = payload::runnable();
    let slice_millis = timed(|| {
        for spec in &specs {
            let _ = measure_exactness(config, spec, &slice, Interference::Quiet);
        }
    });
    records.push(projection(
        "exactness",
        slice.reps * specs.len() as u64,
        slice_millis,
        plan.reps * specs.len() as u64 * plan.conditions.len() as u64,
    ));

    for condition in &plan.conditions {
        for spec in &specs {
            records.extend(measure_exactness(config, spec, plan, *condition)?);
        }
    }

    let mut skid = SkidBuckets::default();
    for spec in &specs {
        let mut idx = 0u64;
        for period in &plan.periods {
            let slice_millis = timed(|| {
                let _ = measure_overflow(config, spec, *period, 32, 0);
            });
            records.push(projection(
                &format!("overflow[{}@{period}]", spec.name),
                32,
                slice_millis,
                plan.arms_per_period,
            ));
            let (armed, buckets) =
                measure_overflow(config, spec, *period, plan.arms_per_period, idx)?;
            idx += plan.arms_per_period;
            records.extend(armed);
            for (bucket, count) in buckets.counts.iter().enumerate() {
                skid.counts[bucket] += count;
            }
            skid.max = skid.max.max(buckets.max);
            skid.min = match (skid.min, buckets.min) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            skid.total += buckets.total;
        }
    }

    let (guest_records, unmeasured) = guest_half(config, plan)?;
    records.extend(guest_records);

    Ok(Stage1Outcome {
        records,
        skid,
        unmeasured,
    })
}

/// The guest half: count exactness read through a vCPU, then the save/restore
/// fixpoint over that vCPU's state. Returns its records and whatever it could
/// not measure.
#[cfg(target_arch = "x86_64")]
fn guest_half(
    config: u64,
    plan: &MeasurementPlan,
) -> Result<(Vec<Record>, Vec<String>), Stage1Error> {
    let slice = plan.slice(1, 32);
    let slice_millis = timed(|| {
        let _ = crate::guest_sys::measure_guest_exactness(config, &slice);
    });
    let mut records = vec![projection(
        "guest_exactness",
        slice.reps,
        slice_millis,
        plan.reps,
    )];
    records.extend(crate::guest_sys::measure_guest_exactness(config, plan)?);
    records.push(crate::guest_sys::measure_fixpoint()?);
    Ok((records, Vec::new()))
}

/// The guest half is built for x86-64, so elsewhere it is reported unmeasured
/// rather than skipped.
#[cfg(not(target_arch = "x86_64"))]
fn guest_half(
    _config: u64,
    _plan: &MeasurementPlan,
) -> Result<(Vec<Record>, Vec<String>), Stage1Error> {
    Ok((
        Vec::new(),
        vec![crate::stage1::GUEST_WINDOW_NOT_BUILT.to_string()],
    ))
}

/// How long `body` took, in milliseconds.
fn timed(body: impl FnOnce()) -> u64 {
    // not order-observable: the projection discipline needs elapsed wall time to
    // extrapolate a campaign's duration. It reaches a `Projection` record and
    // nothing else — no count, no verdict, no hash.
    #[allow(clippy::disallowed_methods)]
    let start = std::time::Instant::now();
    body();
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_load_that_was_never_started_still_stops_cleanly() {
        let load = Load::none();
        drop(load);
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "the load thread pins itself, and Miri has no sched_getcpu"
    )]
    fn a_spinning_load_stops_when_its_guard_drops() {
        let load = Load::spinning_on(0);
        let stop = Arc::clone(&load.stop);
        drop(load);
        assert!(stop.load(Ordering::Relaxed));
    }
}
