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

/// A configured, V-time-wired mock VM with the canonical-blob hash wired and a
/// distinctive guest image loaded — **not yet entered** (the caller decides
/// whether to advance it; a restore target must not be).
pub fn vmm(script: Vec<Exit>, seed: u64) -> Result<Vmm<MockBackend>, VmmError> {
    let mut backend = MockBackend::with_exits(script);
    backend.set_cpuid(&vmm_backend::CpuidModel::default())?;
    backend.set_msr_filter(&vmm_backend::MsrFilter::default())?;
    let mut vmm = Vmm::new(backend, GuestRam::new(RAM)?);
    vmm.wire_vtime(VtimeWiring::new(
        contract_vclock_config(),
        Box::new(TickingWork::new(WORK_STEP)),
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
pub fn server(fork_script: Vec<Exit>) -> Result<ControlServer<MockBackend>, VmmError> {
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
    }
    let factory: VmmFactory<MockBackend> = Box::new(move || vmm(fork_script.clone(), 0));
    Ok(ControlServer::new(live, factory))
}
