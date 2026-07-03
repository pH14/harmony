// SPDX-License-Identifier: AGPL-3.0-or-later
//! A deterministic **toy machine with a planted, fault-triggerable bug** — the
//! portable stand-in (task 60, gate 2) for the box's Postgres-campaign guest.
//!
//! The box milestone plants a real bug in the Postgres workload image, reachable
//! only under an injected host fault, and lets the campaign find it end-to-end
//! against the real control server + patched KVM (gate 1). None of that runs on
//! a laptop. This module gives the campaign loop a **fully controllable guest**
//! so the *finder* — the seed-driven search, the crash oracle mapping, the
//! `Bug` emission, and the N/N replay verification — is exercised bit-for-bit on
//! macOS + Linux, with a planted bug whose trigger we own exactly.
//!
//! ## The planted bug (toy shape)
//!
//! [`ToyPlantedMachine`] models a supervised process whose bookkeeping invariant
//! holds under every nominal execution and is violated only by a **single-event
//! upset** — a [`CorruptMemory`](environment::HostFault::CorruptMemory) that
//! flips the exact `(gpa, mask)` of the supervisor's ledger word at a
//! [`Moment`](environment::Moment) inside a narrow sensitive window. Under that
//! adversity, and only then, the supervisor detects the impossible state and
//! aborts with a distinctive crash; otherwise it reaches the workload's ordinary
//! terminal. This mirrors the box bug's *finder-visible* contract exactly (see
//! `guest/linux/campaign-init.sh`): the campaign never learns the trigger — it
//! searches `(gpa, mask, Moment)` schedules until one crashes.
//!
//! ## Terminal convention (mirrors the box, `guest/linux/campaign-init.sh`)
//!
//! A guest process cannot reach the isa-debug-exit port on the kata-derived
//! container kernel (no `CONFIG_X86_IOPL_IOPERM` / `CONFIG_DEVPORT`), so the
//! planted bug cannot signal a distinct `Crash{Panic}`. Instead the workload's
//! `/init` maps the outcome to two distinct guest terminals the kernel produces:
//! the bug **reboots** (`reboot -f` → backend `Shutdown` → `Crash{Shutdown}`)
//! and a clean run **halts** (`halt -f` → HLT → `Quiescent`). The toy reproduces
//! exactly that: a triggered run yields a [`Crash`](StopReason::Crash) (leading
//! info byte [`CRASH_KIND_SHUTDOWN`], the reboot), and a clean run yields
//! [`Quiescent`](StopReason::Quiescent). The campaign's
//! [`CampaignOracle`](crate::campaign::CampaignOracle) keys on the terminal
//! **class** (any `Crash` is the bug), so the *identical* oracle runs against the
//! toy and the real guest.
//!
//! ## Determinism
//!
//! Every method is a pure function of `(base snapshot, branch env)`: the
//! terminal outcome is a pure function of the branch env's host schedule, the
//! terminal V-time is a pure (integer-hash) function of the env bytes, and the
//! 32-byte `state_hash` is `sha256` of a domain tag + the env bytes + the
//! terminal encoding. So a fixed env replays to a byte-identical
//! `(StopReason, state_hash)` every time — the N/N property the milestone
//! verifies — and distinct envs diverge to distinct hashes.

use std::collections::BTreeMap;

use environment::HostFault;
use explorer::{
    AdapterEnv, Answer, Environment, Machine, MachineError, SnapId, StopConditions, StopReason,
    VTime,
};
use sha2::{Digest, Sha256};

use crate::campaign::CRASH_KIND_SHUTDOWN;

/// The V-time (retired-branch count) the toy guest is quiescent at when first
/// snapshotted — an arbitrary non-zero anchor so a met-deadline `run` (the
/// [`probe_vtime`](crate::probe_vtime) trick) reports a truthful current time.
pub const BASE_VTIME: u64 = 1_000;

/// The **planted trigger**: the exact single-event upset the supervised process
/// cannot survive. The campaign is constructed *without* this — it searches a
/// space of `(gpa, mask, Moment)` schedules until one matches — so the toy holds
/// it privately and the finder earns the crash.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Trigger {
    /// The guest-physical address of the supervisor's ledger word.
    pub gpa: u64,
    /// The XOR bit pattern that flips the ledger's guard/sign bit (a
    /// single-event upset: exactly one bit).
    pub mask: u64,
    /// The half-open [`Moment`](environment::Moment) window `[lo, hi)` during
    /// which the ledger is live and an upset corrupts it (outside it the
    /// supervisor has not yet written / has already checked the word, so the
    /// same upset is inert — the "ordering assumption" the bug encodes).
    pub window: (u64, u64),
}

impl Trigger {
    /// The canonical portable trigger, matched by
    /// [`CampaignConfig::toy`](crate::campaign::CampaignConfig::toy)'s search
    /// space: the ledger word at gpa `0x3000`, the guard bit `31`, and the
    /// one-slot sensitive window at offset `3` past the base. The single point
    /// (of 128 in the toy search space) that fires — the finder must discover
    /// it, the campaign is built without it.
    pub fn toy() -> Self {
        Self {
            gpa: 0x3000,
            mask: 1 << 31,
            window: (BASE_VTIME + 3, BASE_VTIME + 4),
        }
    }

    /// Whether `fault` at `moment` is the planted single-event upset: the exact
    /// ledger `gpa`, the exact guard-bit `mask`, inside the sensitive window.
    fn fires(&self, moment: u64, fault: &HostFault) -> bool {
        match fault {
            HostFault::CorruptMemory { gpa, mask } => {
                *gpa == self.gpa
                    && mask.0 == self.mask
                    && moment >= self.window.0
                    && moment < self.window.1
            }
            _ => false,
        }
    }
}

/// A sealed toy snapshot: the quiescent V-time and the environment active at
/// capture (so a verbatim [`Machine::replay`] restores the exact reproducer, and
/// a [`Machine::branch`] overwrites it with the branch env).
#[derive(Clone, Debug)]
struct Snap {
    vtime: u64,
    env: Environment,
}

/// A deterministic in-process [`Machine`] with a planted, fault-triggerable bug
/// (see the module doc). The portable gate drives the campaign against it; the
/// box gate swaps it for the real socket `Machine` + Postgres-campaign image
/// with **zero campaign-code change**.
pub struct ToyPlantedMachine {
    /// The private planted trigger the finder must discover.
    trigger: Trigger,
    /// The environment active in the (virtual) guest right now — set by
    /// `branch`/`replay`, read by `run`/`hash`/`recorded_env`.
    current: Environment,
    /// The current quiescent V-time (advances on `run`, restored on
    /// `branch`/`replay`).
    vtime: u64,
    /// Sealed snapshots by raw handle.
    snaps: BTreeMap<u64, Snap>,
    /// The next snapshot handle to mint (monotone, never reused).
    next_snap: u64,
}

impl ToyPlantedMachine {
    /// A fresh toy guest, quiescent at [`BASE_VTIME`], with the given planted
    /// `trigger`. Its initial environment is a canonical empty seeded blob (the
    /// "boot" reproducer), so a base `snapshot` + `hash` before any branch is
    /// well-defined.
    pub fn new(trigger: Trigger) -> Self {
        Self {
            trigger,
            current: boot_env(),
            vtime: BASE_VTIME,
            snaps: BTreeMap::new(),
            next_snap: 1,
        }
    }

    /// Decode the active env's host-fault schedule and report whether any staged
    /// fault is the planted single-event upset (the supervised process aborts
    /// iff so). A malformed adapter blob decodes to no schedule → never fires
    /// (the same fail-safe the real supervisor has: no upset, no bug).
    fn triggered(&self) -> bool {
        let Ok(decoded) = AdapterEnv::decode(&self.current) else {
            return false;
        };
        decoded
            .spec
            .host_faults()
            .any(|(m, f)| self.trigger.fires(m, &f))
    }

    /// The deterministic V-time offset the run advances by before terminating —
    /// a pure integer hash of the env bytes, so identical envs reach an
    /// identical terminal V-time (the N/N property) while distinct envs land at
    /// (almost always) distinct times. Kept small so it never oversteps a
    /// realistic deadline.
    fn terminal_offset(&self) -> u64 {
        (fold64(&self.current.bytes) % 4096) + 1
    }
}

/// The canonical "boot" environment: a genesis-complete, fault-free seeded blob
/// keyed at offset zero (the toy's pre-branch state).
fn boot_env() -> Environment {
    AdapterEnv {
        base_offset: 0,
        pos: 0,
        spec: environment::EnvSpec::Seeded {
            seed: 0,
            policy: environment::FaultPolicy::none(),
        },
    }
    .encode()
}

/// A tiny order-independent 64-bit fold (FNV-1a) of some bytes — a deterministic
/// integer hash for the toy's terminal V-time. Not cryptographic; the
/// `state_hash` uses `sha256`.
fn fold64(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

impl Machine for ToyPlantedMachine {
    fn branch(&mut self, snap: SnapId, env: &Environment) -> Result<(), MachineError> {
        let Some(base) = self.snaps.get(&snap.0) else {
            return Err(MachineError::UnknownSnapshot(snap.0));
        };
        // A branch env the adapter could not have minted is a caller bug,
        // surfaced loudly before it touches the (virtual) guest.
        AdapterEnv::decode(env)?;
        self.vtime = base.vtime;
        self.current = env.clone();
        Ok(())
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
        let Some(base) = self.snaps.get(&snap.0) else {
            return Err(MachineError::UnknownSnapshot(snap.0));
        };
        self.vtime = base.vtime;
        self.current = base.env.clone();
        Ok(())
    }

    fn run(
        &mut self,
        until: &StopConditions,
        _resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError> {
        // Deadline already met → stop immediately without advancing (the
        // `probe_vtime` idiom, and a truthful V-time stamp).
        if let Some(d) = until.deadline
            && d.0 <= self.vtime
        {
            return Ok(StopReason::Deadline {
                vtime: VTime(self.vtime),
            });
        }
        // Where the terminal would land. If a (future) deadline falls *before*
        // it, the real `Machine` stops at the deadline, not the terminal — so
        // the toy must too (mock fidelity is what the portable gate certifies).
        // Advance to the deadline and return `Deadline`, leaving the run
        // resumable exactly as the real substrate would.
        let terminal_vtime = self.vtime.saturating_add(self.terminal_offset());
        if let Some(d) = until.deadline
            && d.0 < terminal_vtime
        {
            self.vtime = d.0;
            return Ok(StopReason::Deadline { vtime: VTime(d.0) });
        }
        // Advance to the guest's terminal, mirroring the box terminal convention
        // (`guest/linux/campaign-init.sh`): the supervised process aborts iff the
        // planted single-event upset is staged → `/init` reboots → `Crash{Shutdown}`
        // (the bug); otherwise the loop completes → `/init` halts → `Quiescent`
        // (the clean terminal). The campaign oracle keys on this class.
        let bug = self.triggered();
        self.vtime = terminal_vtime;
        Ok(if bug {
            StopReason::Crash {
                // The box maps the bug's reboot to Crash{Shutdown}; the `0x60`
                // byte after the kind tags the task-60 planted crash.
                vtime: VTime(self.vtime),
                info: vec![CRASH_KIND_SHUTDOWN, 0x60],
            }
        } else {
            StopReason::Quiescent {
                vtime: VTime(self.vtime),
            }
        })
    }

    fn snapshot(&mut self) -> Result<SnapId, MachineError> {
        let id = self.next_snap;
        self.next_snap += 1;
        self.snaps.insert(
            id,
            Snap {
                vtime: self.vtime,
                env: self.current.clone(),
            },
        );
        Ok(SnapId(id))
    }

    fn drop_snap(&mut self, snap: SnapId) -> Result<(), MachineError> {
        match self.snaps.remove(&snap.0) {
            Some(_) => Ok(()),
            None => Err(MachineError::UnknownSnapshot(snap.0)),
        }
    }

    fn hash(&mut self) -> Result<[u8; 32], MachineError> {
        // The state hash is a pure function of the active env and its terminal
        // outcome, so a fixed reproducer hashes identically every replay and
        // distinct reproducers diverge.
        let mut h = Sha256::new();
        h.update(b"conductor.toy.planted.state_hash.v1");
        h.update((self.current.bytes.len() as u64).to_le_bytes());
        h.update(&self.current.bytes);
        h.update([if self.triggered() { 1 } else { 0 }]);
        Ok(h.finalize().into())
    }

    fn coverage(&self) -> &[u8] {
        // The toy exposes no coverage map (the campaign is seed-driven; the
        // oracle keys on the terminal, not coverage).
        &[]
    }

    fn recorded_env(&self) -> Result<Environment, MachineError> {
        // A genesis-rooted run's reproducer already is genesis-complete: return
        // the exact branch env, so `recorded_env` and the branched env agree and
        // either replays identically.
        Ok(self.current.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::campaign::mint_fault_env;
    use environment::{BitMask, EnvSpec, FaultPolicy};
    use explorer::{EnvCodec, SpecEnvCodec};

    /// The canonical toy trigger (matches [`Trigger::toy`]).
    fn trigger() -> Trigger {
        Trigger::toy()
    }

    /// Build a one-fault branch env directly (bypassing the campaign minter).
    fn fault_env(gpa: u64, mask: u64, at: u64) -> Environment {
        let mut spec = EnvSpec::Seeded {
            seed: 7,
            policy: FaultPolicy::none(),
        };
        spec.perturb(
            HostFault::CorruptMemory {
                gpa,
                mask: BitMask(mask),
            },
            at,
        );
        AdapterEnv {
            base_offset: 0,
            pos: 0,
            spec,
        }
        .encode()
    }

    fn base(m: &mut ToyPlantedMachine) -> SnapId {
        m.snapshot().expect("boot is quiescent")
    }

    /// The exact planted upset crashes (reboot → Crash{Shutdown}); a clean run
    /// halts (Quiescent). The oracle keys on that class distinction.
    #[test]
    fn planted_upset_crashes_nominal_halts() {
        let mut m = ToyPlantedMachine::new(trigger());
        let b = base(&mut m);
        let until = StopConditions::default();

        // The exact trigger → Crash (the bug's reboot).
        m.branch(b, &fault_env(0x3000, 1 << 31, BASE_VTIME + 3))
            .unwrap();
        match m.run(&until, None).unwrap() {
            StopReason::Crash { info, .. } => assert_eq!(info[0], CRASH_KIND_SHUTDOWN),
            other => panic!("expected a Crash, got {other:?}"),
        }

        // No faults at all → the clean halt (Quiescent).
        m.branch(b, &SpecEnvCodec.seeded(9)).unwrap();
        match m.run(&until, None).unwrap() {
            StopReason::Quiescent { .. } => {}
            other => panic!("expected a Quiescent halt, got {other:?}"),
        }
    }

    /// Each near-miss on the trigger is inert (wrong gpa, wrong mask bit, or
    /// outside the sensitive Moment window) — it halts (Quiescent), no crash.
    #[test]
    fn near_misses_do_not_fire() {
        let mut m = ToyPlantedMachine::new(trigger());
        let b = base(&mut m);
        let until = StopConditions::default();
        let crashes = |m: &mut ToyPlantedMachine, env: &Environment| {
            m.branch(b, env).unwrap();
            matches!(m.run(&until, None).unwrap(), StopReason::Crash { .. })
        };
        assert!(crashes(&mut m, &fault_env(0x3000, 1 << 31, BASE_VTIME + 3)));
        assert!(
            !crashes(&mut m, &fault_env(0x2000, 1 << 31, BASE_VTIME + 3)),
            "wrong gpa"
        );
        assert!(
            !crashes(&mut m, &fault_env(0x3000, 1 << 30, BASE_VTIME + 3)),
            "wrong bit"
        );
        assert!(
            !crashes(&mut m, &fault_env(0x3000, 1 << 31, BASE_VTIME + 9)),
            "outside window"
        );
    }

    /// A fixed reproducer replays to a byte-identical `(StopReason, state_hash)`
    /// every time — the N/N milestone property at the machine level.
    #[test]
    fn a_fixed_reproducer_replays_identically() {
        let mut m = ToyPlantedMachine::new(trigger());
        let b = base(&mut m);
        let until = StopConditions::default();
        let env = fault_env(0x3000, 1 << 31, BASE_VTIME + 3);
        let mut seen: Option<(StopReason, [u8; 32])> = None;
        for _ in 0..25 {
            m.branch(b, &env).unwrap();
            let stop = m.run(&until, None).unwrap();
            let hash = m.hash().unwrap();
            match &seen {
                None => seen = Some((stop, hash)),
                Some((s0, h0)) => {
                    assert_eq!(&stop, s0, "stop diverged across replays");
                    assert_eq!(&hash, h0, "state_hash diverged across replays");
                }
            }
        }
    }

    /// **Mock fidelity** (the portable gate certifies the toy's `StopConditions`
    /// semantics): a FUTURE deadline that falls *before* the run's terminal makes
    /// the toy stop at `Deadline` (V-time = the deadline) exactly as the real
    /// `Machine` would — it does not overshoot to the terminal. A deadline
    /// at/after the terminal still reaches the terminal.
    #[test]
    fn a_future_deadline_before_the_terminal_stops_there() {
        let mut m = ToyPlantedMachine::new(trigger());
        let b = base(&mut m);
        // A non-triggering env whose terminal advances >= 2 past the base, so an
        // interior future deadline exists. Deterministic search over fixed seeds
        // (offset is 1 only when the env hash is a multiple of 4096 — rare).
        let (env, terminal) = (0u64..64)
            .find_map(|s| {
                let e = fault_env(0x2000, 1 << (s % 8), BASE_VTIME + (s % 4));
                m.branch(b, &e).unwrap();
                let t = m.run(&StopConditions::default(), None).unwrap().vtime().0;
                (t >= BASE_VTIME + 2).then_some((e, t))
            })
            .expect("an env whose terminal advances >= 2");

        // A deadline strictly inside (base, terminal): stop AT the deadline.
        let deadline = terminal - 1;
        let until = StopConditions {
            deadline: Some(VTime(deadline)),
            ..StopConditions::default()
        };
        m.branch(b, &env).unwrap();
        assert_eq!(
            m.run(&until, None).unwrap(),
            StopReason::Deadline {
                vtime: VTime(deadline)
            },
            "a future deadline before the terminal must stop at the deadline, not overshoot"
        );

        // A deadline at/after the terminal still reaches the terminal (a clean
        // Quiescent halt — the env is non-triggering).
        let until_far = StopConditions {
            deadline: Some(VTime(terminal + 100)),
            ..StopConditions::default()
        };
        m.branch(b, &env).unwrap();
        assert!(matches!(
            m.run(&until_far, None).unwrap(),
            StopReason::Quiescent { .. }
        ));
    }

    /// Distinct reproducers diverge to distinct `state_hash`es (so the search
    /// explores genuinely different states, and the divergence gate is real).
    #[test]
    fn distinct_envs_diverge() {
        let mut m = ToyPlantedMachine::new(trigger());
        let b = base(&mut m);
        let until = StopConditions::default();
        let mut hashes = std::collections::BTreeSet::new();
        for seed in 0..16u64 {
            let env = mint_fault_env(BASE_VTIME, seed, &crate::campaign::CampaignConfig::toy());
            m.branch(b, &env).unwrap();
            m.run(&until, None).unwrap();
            hashes.insert(m.hash().unwrap());
        }
        assert!(hashes.len() > 1, "distinct branch envs must diverge");
    }

    /// `branch`/`replay`/`drop` reject an unknown handle; `replay` restores the
    /// captured env verbatim.
    #[test]
    fn handle_discipline_and_replay_verbatim() {
        let mut m = ToyPlantedMachine::new(trigger());
        assert_eq!(
            m.branch(SnapId(99), &boot_env()),
            Err(MachineError::UnknownSnapshot(99))
        );
        assert_eq!(m.replay(SnapId(99)), Err(MachineError::UnknownSnapshot(99)));
        assert_eq!(
            m.drop_snap(SnapId(99)),
            Err(MachineError::UnknownSnapshot(99))
        );

        let b = base(&mut m);
        let env = fault_env(0x3000, 1 << 31, BASE_VTIME + 3);
        m.branch(b, &env).unwrap();
        assert_eq!(m.recorded_env().unwrap(), env);
        // Replaying the base restores the boot env captured at snapshot time.
        m.replay(b).unwrap();
        assert_eq!(m.recorded_env().unwrap(), boot_env());
        m.drop_snap(b).unwrap();
        assert_eq!(m.drop_snap(b), Err(MachineError::UnknownSnapshot(b.0)));
    }
}
