// SPDX-License-Identifier: AGPL-3.0-or-later
//! **Task-78 portable proptest gate**: over random chains with RDRAND draws in
//! random intervals, the compose-folded replay is **bit-identical** to the
//! hop-by-hop original — always. This is the property the reseed markers exist
//! for: every hop's `branch` reseed is recorded at its Moment, `compose`
//! splices the markers positionally, and the `ControlServer` re-executes each
//! collapsed hop's reseed at its exact recorded position (the task-59
//! exact-arrival plane).
//!
//! Everything runs over the real wire — `SocketMachine` + the production
//! [`SpecEnvCodec`] against vmm-core's `ControlServer` over the mock
//! composition — so the whole record → compose → re-anchor → re-execute chain
//! is exercised per case, not a model of it.
//!
//! **Mock constraint (script-restart phase).** The scripted mock restarts its
//! exit script from index 0 at every `branch` (the script position is not part
//! of `VcpuState`) — a phase artifact a real guest does not have: two legs
//! reaching the same V-time span through different branch points see the
//! script at different offsets, so a non-uniform draw pattern gives them
//! different draw COUNTS and the comparison stops measuring the splice. The
//! script here is therefore period-400: `RDTSC, RDRAND, RDTSC, RDTSC` per
//! period (a draw mid-period, a sealable RDTSC boundary at the period end),
//! and hop deadlines are drawn as `400·k − jitter` (requested deadlines
//! deliberately OFF the intercept grid; the landed, sealed boundary is the
//! 400-multiple) — every branch floor is then ≡ the chain start mod 400, so a
//! restarted script reproduces the global draw pattern exactly and the two
//! legs execute identical draw sequences, at any chain depth. The randomness
//! lives in the chain shape — depth, per-hop spans, jitter, and the seed —
//! which puts the draws inside every collapsed interval at varying distances
//! from the reseed markers: the gate's "draws in random intervals". The
//! unconstrained-shape divergence is this mock artifact only; the box gate
//! (`live_materialization.rs`) covers the real guest, whose instruction
//! stream does not restart at a branch.

use campaign_runner::mock;
use campaign_runner::{probe_vtime, run_session};
use environment::{EnvSpec, FaultPolicy};
use explorer::adapter::SocketMachine;
use explorer::{EnvCodec, Machine, Moment, SpecEnvCodec, StopConditions, StopMask, StopReason};
use proptest::prelude::*;

/// The env the mock live VM boots under.
fn boot_env() -> EnvSpec {
    EnvSpec::Seeded {
        seed: mock::BOOT_SEED,
        policy: FaultPolicy::none(),
    }
}

/// The period-400 draw-carrying fork script (module doc): `periods` × (RDTSC,
/// RDRAND, RDTSC, RDTSC), then a clean Hlt.
fn period4_script(periods: usize) -> Vec<vmm_backend::Exit> {
    use vmm_backend::Exit;
    let mut out = Vec::with_capacity(periods * 4 + 1);
    for _ in 0..periods {
        out.push(Exit::Rdtsc);
        out.push(Exit::Rdrand { width: 8 });
        out.push(Exit::Rdtsc);
        out.push(Exit::Rdtsc);
    }
    out.push(Exit::Idle);
    out
}

fn config(cases: u32) -> ProptestConfig {
    let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 4 } else { cases });
    cfg.max_shrink_iters = 64;
    cfg
}

proptest! {
    #![proptest_config(config(256))]

    /// fold == hop-by-hop, always: an arbitrary chain depth, per-hop span
    /// (with off-grid requested deadlines), and seed round-trips
    /// bit-identically through one compose-folded branch, with RDRAND draws
    /// landing inside every collapsed hop.
    #[test]
    fn draw_carrying_folds_are_bit_identical(
        hops in 2usize..=4,
        spans in prop::collection::vec((1u64..=2, 0u64..=99), 4),
        seed in any::<u64>(),
    ) {
        // Requested deadline = 400·k − jitter: off-grid, landing on the 400-
        // multiple boundary (see the module doc's phase constraint).
        let deltas: Vec<u64> = spans.iter().map(|&(k, jitter)| 400 * k - jitter).collect();
        let mut server = mock::server(period4_script(18)).expect("mock server");
        let (served, ()) = run_session(&mut server, move |stream| {
            let mut m = SocketMachine::connect(stream, boot_env()).expect("connect");
            let codec = SpecEnvCodec;
            let seed_env = codec.seeded(seed);
            let run_to = |m: &mut SocketMachine<_>, deadline: u64| -> u64 {
                let stop = m
                    .run(
                        &StopConditions {
                            deadline: Some(Moment(deadline)),
                            on: StopMask::NONE,
                        },
                        None,
                    )
                    .expect("run");
                match stop {
                    StopReason::Deadline { vtime } => vtime.0,
                    other => panic!("expected a Deadline stop, got {other:?}"),
                }
            };

            let v0 = probe_vtime(&mut m).expect("probe");
            let g = m.snapshot().expect("base seal");

            // The hop-by-hop chain: branch → run(deadline) → seal per hop
            // (retrying past staged-RNG boundaries), hash at the final stop.
            let mut cur = g;
            let mut cur_at = v0;
            let mut fold: Option<explorer::Reproducer> = None;
            let mut h_chain = [0u8; 32];
            for (i, delta) in deltas.iter().take(hops).enumerate() {
                m.branch(cur, &seed_env).expect("branch hop");
                let mut at = run_to(&mut m, cur_at + delta);
                let last = i == hops - 1;
                if !last {
                    let seal = loop {
                        match m.snapshot() {
                            Ok(s) => break s,
                            Err(explorer::MachineError::NotQuiescent) => {
                                at = run_to(&mut m, at + 100);
                            }
                            Err(e) => panic!("hop seal: {e}"),
                        }
                    };
                    cur = seal;
                } else {
                    h_chain = m.hash().expect("chain hash");
                }
                let suffix = m.recorded_env().expect("suffix");
                fold = Some(match fold {
                    None => suffix,
                    Some(prev) => codec
                        .compose(&prev, &suffix)
                        .expect("compose adapter-minted blobs"),
                });
                cur_at = at;
            }
            let fold = fold.expect("hops >= 2");

            // The folded leg: ONE branch from the base over the composed
            // chain, run to the same absolute V-time — bit-identical, always.
            m.branch(g, &fold).expect("branch folded");
            let landed = run_to(&mut m, cur_at);
            assert_eq!(landed, cur_at, "V-time timing is draw-value-independent");
            let h_fold = m.hash().expect("fold hash");
            assert_eq!(
                h_fold, h_chain,
                "task-78 gate: the compose-folded replay must be bit-identical to the \
                 hop-by-hop chain (hops {hops}, deltas {deltas:?}, seed {seed:#x})"
            );
        });
        served.expect("server session");
    }
}
