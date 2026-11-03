// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{
    campaign::{Campaign, Config, Stop},
    target::{Action, Kind, Observation, Position, Probe, Vocabulary, Want, Worker},
};

struct Corridor;

fn decode(state: &[u8]) -> (u64, u64) {
    (u64::from(state[0]), u64::from(state[1]))
}

fn scalars(x: u64, ammo: u64) -> Vec<u64> {
    vec![x, ammo]
}

fn step(x: u64, ammo: u64, input: u32) -> (u64, u64) {
    match input {
        0 if x > 0 => (x - 1, ammo),
        1 if x < 200 => {
            if x == 120 && ammo < 6 {
                (x, ammo)
            } else {
                (x + 1, ammo)
            }
        }
        2 if x == 10 => (x, (ammo + 1).min(9)),
        3 if ammo > 0 => (x, ammo - 1),
        _ => (x, ammo),
    }
}

impl Worker for Corridor {
    fn observe_start(&mut self, start: &[u8]) -> Result<Observation, String> {
        let (x, a) = decode(start);
        Ok(Observation {
            scalars: scalars(x, a),
            array: start.to_vec(),
            milestone: false,
        })
    }

    fn run(
        &mut self,
        start: &[u8],
        actions: &[Action],
        probe: &mut dyn Probe,
    ) -> Result<u64, String> {
        let (mut x, mut a) = decode(start);
        let mut work = 0;
        for (i, act) in actions.iter().enumerate() {
            for s in 0..act.hold {
                (x, a) = step(x, a, act.input);
                work += 1;
                let st = [x as u8, a as u8];
                match probe.observe(
                    Position { action: i, step: s },
                    work,
                    &scalars(x, a),
                    &st,
                    x >= 200,
                ) {
                    Want::Nothing => {}
                    Want::Capture => probe.keep(st.to_vec()),
                    Want::Stop => return Ok(work),
                }
            }
        }
        Ok(work)
    }
}

fn campaign(seed: u64, budget: u64) -> Campaign {
    Campaign::new(
        vec![Kind::Identity, Kind::Higher],
        Vocabulary {
            inputs: 4,
            holds: true,
            base_steps: 16,
        },
        Config::new(seed, budget),
        vec![0, 0],
        scalars(0, 0),
    )
}

#[test]
fn reaches_door_by_carrying_ammo_and_stream_ignores_worker_count() {
    let mut hashes = Vec::new();
    for workers in [1usize, 3] {
        let mut c = campaign(5, 50_000_000);
        let mut ws: Vec<Box<dyn Worker>> = (0..workers)
            .map(|_| Box::new(Corridor) as Box<dyn Worker>)
            .collect();
        let out = c.run(&mut ws, &mut |_| true, &mut |_| {});
        assert_eq!(out.stop, Stop::Milestone, "{out:?}");
        let route = c.route(out.milestone_state.unwrap());
        let (mut x, mut a) = (0, 0);
        for act in &route {
            for _ in 0..act.hold {
                (x, a) = step(x, a, act.input);
            }
        }
        assert!(x >= 200, "route replays to x {x}");
        hashes.push((out.job_stream_sha256, out.work));
    }
    assert_eq!(hashes[0], hashes[1]);
}

#[test]
fn smaller_budget_is_a_prefix() {
    let run = |budget| {
        let mut c = campaign(9, budget);
        let mut ws: Vec<Box<dyn Worker>> = vec![Box::new(Corridor), Box::new(Corridor)];
        c.run(&mut ws, &mut |_| true, &mut |_| {})
    };
    let big = run(u64::MAX);
    let big2 = run(big.work + 1_000_000);
    assert_eq!(big.job_stream_sha256, big2.job_stream_sha256);
    assert_eq!(big.work, big2.work);
    let small = run(big.work / 2);
    assert_eq!(small.stop, Stop::Budget);
    assert!(small.work >= big.work / 2 && small.work < big.work);
}
