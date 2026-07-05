// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **portable mock composition**: a scripted `MockBackend` guest behind
//! the control-transport server, with a monotone work source so V-time
//! actually advances — what the loopback gates and the demo's `mock` mode run
//! against on macOS/Linux with no `/dev/kvm`.
//!
//! The scripted guest is deterministic by construction: every fork VM gets the
//! same exit script, work advances by a fixed step at each V-time intercept,
//! and RDRAND draws come from the VMM's seeded stream — so a branch's future
//! is a pure function of `(script, seed)`, exactly the property the gates pin.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vmm_backend::{Backend, Exit, MockBackend};
use vmm_core::control::{ControlServer, VmmFactory};
use vmm_core::vmm::{GuestRam, Step, Vmm, VmmError, VtimeWiring, contract_vclock_config};
use vmm_core::work::{WorkError, WorkSource};

/// Guest RAM for the mock guest: 16 KiB (4 pages) — enough for a distinctive
/// image, cheap enough for 256-case property tests.
pub const RAM: usize = 0x4000;

/// The mock live VM's boot seed (the env the composition root reports to the
/// adapter as the initial environment).
pub const BOOT_SEED: u64 = 0xC0_FF_EE;

/// A portable, monotone [`WorkSource`]: every read advances the count by a
/// fixed `step`, modelling a guest that retires `step` conditional branches
/// between consecutive V-time intercepts. Deterministic for a fixed exit
/// script (work is read only at intercept completions), which is all the mock
/// gates need; `reset` (snapshot restore) rewinds to zero like the real
/// counter.
#[derive(Debug)]
pub struct TickingWork {
    work: Cell<u64>,
    step: u64,
}

impl TickingWork {
    /// A source that advances by `step` per read, starting at zero.
    pub fn new(step: u64) -> Self {
        TickingWork {
            work: Cell::new(0),
            step,
        }
    }
}

impl WorkSource for TickingWork {
    fn work(&self) -> Result<u64, WorkError> {
        let next = self.work.get().saturating_add(self.step);
        self.work.set(next);
        Ok(next)
    }

    fn reset(&mut self) -> Result<(), WorkError> {
        self.work.set(0);
        Ok(())
    }
}

/// How far V-time advances per intercept in the mock composition (1 ns per
/// retired branch under the contract clock ⇒ 100 ns per intercept).
pub const WORK_STEP: u64 = 100;

/// The shared work state behind the mock composition (task 78): `natural` is
/// `WORK_STEP ×` the number of scripted exits the backend has serviced (the
/// guest's "retired branches" — advanced ONLY by guest execution, exactly like
/// the box's `perf_event` counter, so host-side bookkeeping reads are
/// cadence-neutral); `arrival` is the transient exact-arrival position an armed
/// `run_until` stopped at between exits (cleared by the next serviced exit).
#[derive(Debug, Default)]
pub struct SharedWork {
    natural: AtomicU64,
    arrival: AtomicU64,
}

impl SharedWork {
    fn current(&self) -> u64 {
        self.natural
            .load(Ordering::Relaxed)
            .max(self.arrival.load(Ordering::Relaxed))
    }

    /// One scripted exit serviced: the guest ran to its next natural boundary.
    fn on_exit(&self) {
        // Saturating (PR #62 round-6): a grid already driven to the top of the
        // axis (an on_deadline at/near u64::MAX) must pin there, never wrap a
        // release build back to a tiny work count (or panic a debug one).
        let n = self
            .natural
            .load(Ordering::Relaxed)
            .saturating_add(WORK_STEP);
        self.natural.store(n, Ordering::Relaxed);
        // The natural grid has caught up with (or passed) any arrival point
        // (`run_until` only stages an arrival strictly below natural + step).
        self.arrival.store(0, Ordering::Relaxed);
    }

    /// A **scripted** `Deadline` serviced through the delegated far-deadline
    /// `run_until` (PR #62 round-5 fix): the guest ran to exactly `reached`,
    /// so the natural grid advances TO it (monotone), not by one step — else
    /// the next intercept would sample work below the V-time vmm-core just
    /// anchored from `reached`.
    fn on_deadline(&self, reached: u64) {
        let n = self.natural.load(Ordering::Relaxed).max(reached);
        self.natural.store(n, Ordering::Relaxed);
        self.arrival.store(0, Ordering::Relaxed);
    }
}

/// A pure-reader [`WorkSource`] over the [`SharedWork`] the backend advances.
/// Reads never tick — the counter models guest execution only — so the armed
/// (exact-arrival) and unarmed paths observe identical V-time cadences, the
/// property the task-78 reseed markers (and any staged host fault) need for
/// bit-identical folds on this composition.
#[derive(Debug)]
pub struct CountedWork(Arc<SharedWork>);

impl WorkSource for CountedWork {
    fn work(&self) -> Result<u64, WorkError> {
        Ok(self.0.current())
    }

    fn reset(&mut self) -> Result<(), WorkError> {
        self.0.natural.store(0, Ordering::Relaxed);
        self.0.arrival.store(0, Ordering::Relaxed);
        Ok(())
    }
}

/// A [`MockBackend`] wrapper that advances the [`SharedWork`] counter per
/// serviced scripted exit and implements **exact arrival**: an armed
/// `run_until(d)` whose deadline falls before the next natural boundary stops
/// *between exits* at exactly `d` (a `Deadline` exit, no script consumed) —
/// the mock analogue of the box's armed PMU stop. An at-or-past deadline is
/// the round-13 zero-step (`reached == current`, no entry).
pub struct CountingBackend {
    inner: MockBackend,
    work: Arc<SharedWork>,
}

impl Backend for CountingBackend {
    fn set_cpuid(&mut self, m: &vmm_backend::CpuidModel) -> vmm_backend::Result<()> {
        self.inner.set_cpuid(m)
    }
    fn set_msr_filter(&mut self, f: &vmm_backend::MsrFilter) -> vmm_backend::Result<()> {
        self.inner.set_msr_filter(f)
    }
    unsafe fn map_memory(
        &mut self,
        gpa: vmm_backend::Gpa,
        host: &mut [u8],
    ) -> vmm_backend::Result<()> {
        // SAFETY: forwards to the inner mock, which only records the region.
        unsafe { self.inner.map_memory(gpa, host) }
    }
    fn run(&mut self) -> vmm_backend::Result<Exit> {
        let exit = self.inner.run()?;
        self.work.on_exit();
        Ok(exit)
    }
    fn run_until(&mut self, d: vmm_backend::Vtime) -> vmm_backend::Result<Exit> {
        let cur = self.work.current();
        if d.0 <= cur {
            // Zero-step: already at/past the deadline — reached == work-before,
            // no guest entry (round-13).
            return Ok(Exit::Deadline {
                reached: vmm_backend::Vtime(cur),
            });
        }
        if d.0
            <= self
                .work
                .natural
                .load(Ordering::Relaxed)
                .saturating_add(WORK_STEP)
        {
            // Exact arrival between exits: the guest runs to exactly `d` and
            // stops; no scripted exit is consumed and the natural grid is
            // untouched, so the armed leg services the same script at the same
            // work counts as an unarmed run.
            self.work.arrival.store(d.0, Ordering::Relaxed);
            return Ok(Exit::Deadline {
                reached: vmm_backend::Vtime(d.0),
            });
        }
        // Far deadline: the next scripted exit lands first. Delegate to the
        // inner mock's `run_until` — NOT `run` — so a scripted `Exit::Deadline`
        // placeholder is rewritten to `reached = d` (PR #62 round-4 blocking
        // fix: falling through to `run()` handed vmm-core the stale scripted
        // value, mis-anchoring V-time and the schedule/reseed drains).
        let exit = self.inner.run_until(d)?;
        match &exit {
            // A scripted Deadline placeholder was rewritten to `reached = d`:
            // the guest ran to exactly there — advance the grid TO it.
            Exit::Deadline { reached } => self.work.on_deadline(reached.0),
            _ => self.work.on_exit(),
        }
        Ok(exit)
    }
    fn inject(&mut self, e: vmm_backend::Event) -> vmm_backend::Result<()> {
        self.inner.inject(e)
    }
    fn set_pending_irq(&mut self, v: Option<u8>) -> vmm_backend::Result<()> {
        self.inner.set_pending_irq(v)
    }
    fn take_accepted_interrupt(&mut self) -> Option<u8> {
        self.inner.take_accepted_interrupt()
    }
    fn complete_read(&mut self, v: u64) -> vmm_backend::Result<()> {
        self.inner.complete_read(v)
    }
    fn complete_fault(&mut self) -> vmm_backend::Result<()> {
        self.inner.complete_fault()
    }
    fn complete_ok(&mut self) -> vmm_backend::Result<()> {
        self.inner.complete_ok()
    }
    fn complete_hypercall(&mut self, rax: u64) -> vmm_backend::Result<()> {
        self.inner.complete_hypercall(rax)
    }
    fn complete_cpuid(&mut self, a: u32, b: u32, c: u32, d: u32) -> vmm_backend::Result<()> {
        self.inner.complete_cpuid(a, b, c, d)
    }
    fn save(&self) -> vmm_backend::Result<vmm_backend::VcpuState> {
        self.inner.save()
    }
    fn restore(&mut self, s: &vmm_backend::VcpuState) -> vmm_backend::Result<()> {
        self.inner.restore(s)
    }
    fn exit_counts(&self) -> vmm_backend::ExitCounts {
        self.inner.exit_counts()
    }
    fn reset_exit_counts(&mut self) {
        self.inner.reset_exit_counts()
    }
    fn capabilities(&self) -> vmm_backend::Capabilities {
        self.inner.capabilities()
    }
}

/// A configured, V-time-wired mock VM with the canonical-blob hash wired and a
/// distinctive guest image loaded — **not yet entered** (the caller decides
/// whether to advance it; a restore target must not be). Work is the
/// exit-driven [`SharedWork`] counter (task 78): V-time advances only with
/// guest execution, and armed runs stop exactly at their arrival points.
pub fn vmm(script: Vec<Exit>, seed: u64) -> Result<Vmm<CountingBackend>, VmmError> {
    let work = Arc::new(SharedWork::default());
    let mut backend = CountingBackend {
        inner: MockBackend::with_exits(script),
        work: Arc::clone(&work),
    };
    backend.set_cpuid(&vmm_backend::CpuidModel::default())?;
    backend.set_msr_filter(&vmm_backend::MsrFilter::default())?;
    let mut vmm = Vmm::new(backend, GuestRam::new(RAM)?);
    vmm.wire_vtime(VtimeWiring::new(
        contract_vclock_config(),
        Box::new(CountedWork(work)),
        seed,
    )?);
    vmm.wire_snapshot_hashing();
    let mut image = vec![0u8; RAM];
    image[..11].copy_from_slice(b"MOCK_GUEST\n");
    image[2 * 4096] = 0x5A;
    vmm.restore_guest_memory(&image)?;
    Ok(vmm)
}

/// The default fork script: a run that reads the TSC, draws entropy twice
/// (so the branch seed reaches the run through the deterministic RDRAND path),
/// reads the TSC again, and halts cleanly.
pub fn default_fork_script() -> Vec<Exit> {
    vec![
        Exit::Rdtsc,
        Exit::Rdrand { width: 8 },
        Exit::Rdtsc,
        Exit::Rdrand { width: 8 },
        Exit::Rdtsc,
        Exit::Hlt,
    ]
}

/// The fork script for the **task-65 recording** demo: like
/// [`default_fork_script`] but it first writes a recognizable console banner to
/// COM1 (port `0x3F8`), so the recorded [`RunTrace`](explorer::RunTrace) has
/// **non-empty `records`** the scrape decoder splits into lines. The banner is
/// seed-independent (identical across seeds — divergence lives in the env's seed,
/// not the console), then two RDRAND draws carry the seed into the run and a
/// clean `Hlt` terminates it.
pub fn recording_fork_script() -> Vec<Exit> {
    let mut script = vec![Exit::Rdtsc];
    for &b in b"MOCK-READY\n" {
        script.push(Exit::Io {
            port: 0x3F8,
            size: 1,
            write: Some(b as u32),
        });
    }
    script.push(Exit::Rdrand { width: 8 });
    script.push(Exit::Rdrand { width: 8 });
    script.push(Exit::Hlt);
    script
}

/// The fork script for the **task-68 chain protocol**: a long run of V-time
/// intercepts (each RDTSC advances V-time by [`WORK_STEP`] and lands on a
/// sealable synchronized boundary), so a chain of `branch → run(deadline) →
/// seal` hops — and the single long from-genesis fold replay — always finds
/// its boundaries before the clean `Hlt` terminal.
///
/// With `draws`, every other intercept is an RDRAND from the VMM's seeded
/// stream: the script that pins the **sequential-entropy-splice limit** (a
/// compose-fold collapses the per-hop reseed points, so a leg spanning two
/// hops draws a different count/sequence than the hop-by-hop chain did — the
/// round-trip hashes must diverge, documenting the substrate contract
/// boundary escalated by task 68).
pub fn chain_fork_script(intercepts: usize, draws: bool) -> Vec<Exit> {
    let mut script = Vec::with_capacity(intercepts + 1);
    for i in 0..intercepts {
        if draws && !i.is_multiple_of(2) {
            script.push(Exit::Rdrand { width: 8 });
        } else {
            script.push(Exit::Rdtsc);
        }
    }
    script.push(Exit::Hlt);
    script
}

/// Compose the mock control server: a live VM advanced to a synchronized
/// (post-RDTSC) boundary — so the session's first `snapshot` seals first-try —
/// and a factory that boots fork VMs with `fork_script`. Every fork VM is
/// identical by construction, so a branch's future depends only on its seed.
pub fn server(fork_script: Vec<Exit>) -> Result<ControlServer<CountingBackend>, VmmError> {
    let mut live_script = vec![Exit::Rdtsc];
    live_script.extend(fork_script.iter().cloned());
    let mut live = vmm(live_script, BOOT_SEED)?;
    // One step services the leading RDTSC: V-time is synchronized and no RNG
    // completion is staged — a sealable boundary.
    match live.step()? {
        Step::Continued => {}
        Step::Terminal(reason) => {
            return Err(VmmError::ContractViolation(format!(
                "mock live VM terminated at its sync step ({reason:?})"
            )));
        }
        // The mock VM wires no SDK channel (task 73), so the leading-RDTSC sync
        // step can never surface an SDK stop; treat it as a contract violation
        // rather than silently continuing.
        Step::SdkStop => {
            return Err(VmmError::ContractViolation(
                "mock live VM surfaced an unexpected SDK stop at its sync step".to_string(),
            ));
        }
    }
    let factory: VmmFactory<CountingBackend> = Box::new(move || vmm(fork_script.clone(), 0));
    Ok(ControlServer::new(live, factory))
}

#[cfg(test)]
mod tests {
    //! Lib-target unit tests so the crate's one `unsafe` block (the
    //! `CountingBackend::map_memory` forward) and the exact-arrival
    //! `run_until` seam are reachable under the nightly Miri job
    //! (`cargo miri test -p conductor --lib` — the unsafe⇒Miri rule).

    use super::*;
    use vmm_backend::Vtime;

    /// Composing the mock VM exercises the unsafe `map_memory` forward
    /// (GuestRam is mapped into the backend at `Vmm::new`), and a step
    /// advances the exit-driven work counter by exactly one intercept.
    #[test]
    fn mock_vmm_composes_maps_memory_and_ticks_per_exit() {
        let mut v = vmm(vec![Exit::Rdtsc, Exit::Rdtsc, Exit::Hlt], 7).expect("compose");
        assert_eq!(v.step().unwrap(), vmm_core::vmm::Step::Continued);
        let vns1 = v.effective_vns().expect("V-time wired");
        assert_eq!(vns1, WORK_STEP, "one serviced exit = one work step");
        // Bookkeeping reads are cadence-neutral: reading V-time again does
        // not advance it (the task-78 fix over the old TickingWork).
        assert_eq!(v.effective_vns().unwrap(), vns1);
    }

    /// The exact-arrival seam: an armed `run_until` whose deadline falls
    /// before the next natural boundary stops between exits at exactly the
    /// deadline (no scripted exit consumed); the natural grid is untouched,
    /// so the deferred exit still lands at its own boundary.
    #[test]
    fn counting_backend_run_until_arrives_between_exits() {
        let work = Arc::new(SharedWork::default());
        let mut inner = MockBackend::with_exits(vec![Exit::Rdtsc, Exit::Hlt]);
        inner
            .set_cpuid(&vmm_backend::CpuidModel::default())
            .unwrap();
        inner
            .set_msr_filter(&vmm_backend::MsrFilter::default())
            .unwrap();
        let mut b = CountingBackend {
            inner,
            work: Arc::clone(&work),
        };
        // Arrival at 40 (< natural 0 + 100): a Deadline at exactly 40, script
        // untouched, reads observe 40.
        let exit = b.run_until(vmm_backend::Vtime(40)).unwrap();
        assert_eq!(
            exit,
            Exit::Deadline {
                reached: vmm_backend::Vtime(40)
            }
        );
        assert_eq!(work.current(), 40);
        // Zero-step: at-or-past deadline reports reached == current, no entry.
        let exit = b.run_until(vmm_backend::Vtime(40)).unwrap();
        assert_eq!(
            exit,
            Exit::Deadline {
                reached: vmm_backend::Vtime(40)
            }
        );
        // The deferred scripted exit still lands on its natural boundary
        // (100), clearing the transient arrival.
        let exit = b.run_until(vmm_backend::Vtime(500)).unwrap();
        assert_eq!(exit, Exit::Rdtsc);
        assert_eq!(work.current(), WORK_STEP);
    }

    /// The far-deadline path delegates to the inner mock's `run_until`, so a
    /// scripted `Exit::Deadline` placeholder is rewritten to the armed
    /// deadline — never handed through with its stale scripted `reached`
    /// (PR #62 round-4 blocking fix).
    #[test]
    fn counting_backend_far_deadline_rewrites_a_scripted_deadline() {
        let work = Arc::new(SharedWork::default());
        let mut inner =
            MockBackend::with_exits(vec![Exit::Deadline { reached: Vtime(0) }, Exit::Hlt]);
        inner
            .set_cpuid(&vmm_backend::CpuidModel::default())
            .unwrap();
        inner
            .set_msr_filter(&vmm_backend::MsrFilter::default())
            .unwrap();
        let mut b = CountingBackend {
            inner,
            work: Arc::clone(&work),
        };
        // d = 250 is beyond natural + WORK_STEP (0 + 100): the far path must
        // service the scripted Deadline REWRITTEN to reached = 250, not the
        // stale scripted 0.
        let exit = b.run_until(Vtime(250)).unwrap();
        assert_eq!(
            exit,
            Exit::Deadline {
                reached: Vtime(250)
            },
            "a scripted Deadline placeholder is rewritten to the armed deadline"
        );
        assert_eq!(
            work.current(),
            250,
            "the grid advances TO the reached deadline (round-5 fix), not by one step"
        );
    }

    /// After a delegated scripted-Deadline stop, the NEXT intercept samples
    /// work at-or-above the V-time the Deadline anchored (PR #62 round-5
    /// blocking fix): a `[Deadline, Rdtsc]` script must never hand vmm-core a
    /// work count below the recorded `reached`.
    #[test]
    fn counting_backend_next_intercept_samples_at_or_above_the_reached_deadline() {
        let work = Arc::new(SharedWork::default());
        let mut inner = MockBackend::with_exits(vec![
            Exit::Deadline { reached: Vtime(0) },
            Exit::Rdtsc,
            Exit::Hlt,
        ]);
        inner
            .set_cpuid(&vmm_backend::CpuidModel::default())
            .unwrap();
        inner
            .set_msr_filter(&vmm_backend::MsrFilter::default())
            .unwrap();
        let mut b = CountingBackend {
            inner,
            work: Arc::clone(&work),
        };
        let exit = b.run_until(Vtime(250)).unwrap();
        assert_eq!(
            exit,
            Exit::Deadline {
                reached: Vtime(250)
            }
        );
        assert_eq!(work.current(), 250, "grid at the reached deadline");
        // The following Rdtsc intercept reads 350 — never 200 (one step off
        // the pre-fix +WORK_STEP grid, BELOW the anchored 250).
        let exit = b.run().unwrap();
        assert_eq!(exit, Exit::Rdtsc);
        assert_eq!(
            work.current(),
            250 + WORK_STEP,
            "the next intercept samples work above the anchored V-time"
        );
    }

    /// The top of the work axis (PR #62 round-6 blocking fix): an arrival /
    /// delegated Deadline at `u64::MAX` pins the grid there — a subsequent
    /// serviced exit saturates instead of wrapping (release) or panicking
    /// (debug), and a re-armed `run_until` at the pinned top is the zero-step,
    /// never an overflowed boundary comparison.
    #[test]
    fn counting_backend_saturates_at_the_top_of_the_work_axis() {
        let work = Arc::new(SharedWork::default());
        let mut inner = MockBackend::with_exits(vec![
            Exit::Deadline { reached: Vtime(0) },
            Exit::Rdtsc,
            Exit::Hlt,
        ]);
        inner
            .set_cpuid(&vmm_backend::CpuidModel::default())
            .unwrap();
        inner
            .set_msr_filter(&vmm_backend::MsrFilter::default())
            .unwrap();
        let mut b = CountingBackend {
            inner,
            work: Arc::clone(&work),
        };
        // Drive the grid to the top via the delegated scripted Deadline.
        let exit = b.run_until(Vtime(u64::MAX)).unwrap();
        assert_eq!(
            exit,
            Exit::Deadline {
                reached: Vtime(u64::MAX)
            }
        );
        assert_eq!(work.current(), u64::MAX, "grid pinned at the axis top");
        // A serviced exit saturates — never wraps below the anchored V-time.
        let exit = b.run().unwrap();
        assert_eq!(exit, Exit::Rdtsc);
        assert_eq!(work.current(), u64::MAX, "on_exit saturates at u64::MAX");
        // Re-arming at the pinned top is the zero-step (reached == current),
        // exercising the saturating boundary comparison, not an overflow.
        let exit = b.run_until(Vtime(u64::MAX)).unwrap();
        assert_eq!(
            exit,
            Exit::Deadline {
                reached: Vtime(u64::MAX)
            },
            "at-the-top re-arm is a zero-step"
        );
        assert_eq!(work.current(), u64::MAX);
    }
}
