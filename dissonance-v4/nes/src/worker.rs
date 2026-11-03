// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{path::Path, sync::Arc};

use engine::target::{Action, Observation, Position, Probe, Want, Worker};
use machine::{
    Machine, MachineError, Moment, StopConditions,
    nes::{ButtonChord, reproducer, with_cartridge_ram},
    quicknes::QuickNesMachine,
};

use crate::game::{CART_LEN, Game, Predicate};

fn err(e: MachineError) -> String {
    format!("{e:?}")
}

pub struct Milestone {
    pub predicates: Vec<Predicate>,
    pub start: Vec<u8>,
}

pub struct NesWorker {
    pub machine: QuickNesMachine,
    game: Arc<Game>,
    milestone: Arc<Milestone>,
    scalars: Vec<u64>,
    capture: Vec<u8>,
}

impl NesWorker {
    pub fn new(
        game: Arc<Game>,
        milestone: Arc<Milestone>,
        rom: &[u8],
        core: &Path,
        core_sha256: &str,
    ) -> Result<Self, String> {
        let image = if game.cartridge_ram {
            with_cartridge_ram(rom).map_err(err)?
        } else {
            rom.to_vec()
        };
        let mut machine =
            QuickNesMachine::from_rom_bytes(&image, core, core_sha256).map_err(err)?;
        if game.cartridge_ram {
            machine.capture_save_ram_per_frame(0..CART_LEN);
        }
        Ok(Self {
            machine,
            game,
            milestone,
            scalars: Vec::new(),
            capture: Vec::new(),
        })
    }

    pub fn power_on(&mut self) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        self.machine.capture_into(&mut bytes).map_err(err)?;
        Ok(bytes)
    }

    pub fn stage(&mut self, start: &[u8], chords: &[ButtonChord]) -> Result<(), String> {
        let snap = self.machine.import_snapshot(start);
        let staged = self.machine.branch(snap, &reproducer(chords)).map_err(err);
        self.machine.drop_snapshot(snap).map_err(err)?;
        staged
    }

    pub fn frame(&mut self) -> Result<(), String> {
        let deadline = Moment(self.machine.now().0 + 1);
        self.machine
            .run(
                StopConditions {
                    deadline: Some(deadline),
                    ..StopConditions::default()
                },
                None,
            )
            .map_err(err)?;
        if self.machine.frames().is_empty() {
            return Err("staged input ran out".into());
        }
        Ok(())
    }

    pub fn ram(&self) -> (&[u8], &[u8]) {
        let wram = self.machine.frames().last().map(|f| &f[..]).unwrap_or(&[]);
        let cart = if self.game.cartridge_ram {
            let all = self.machine.save_ram_frames();
            &all[all.len().saturating_sub(CART_LEN)..]
        } else {
            &[]
        };
        (wram, cart)
    }

    pub fn milestone_holds(&self) -> bool {
        let (wram, cart) = self.ram();
        let m = &self.milestone;
        !m.predicates.is_empty()
            && m.predicates.iter().all(|p| {
                p.holds(
                    wram,
                    cart,
                    crate::game::read(p.address, &m.start, &m.start[0x800.min(m.start.len())..]),
                )
            })
    }

    pub fn core_state(&self) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        self.machine.capture_into(&mut bytes).map_err(err)?;
        Ok(bytes)
    }

    pub fn chords(&self, actions: &[Action]) -> Vec<ButtonChord> {
        actions
            .iter()
            .map(|a| ButtonChord::new(self.game.buttons(a.input), a.hold.min(120) as u8))
            .collect()
    }
}

impl Worker for NesWorker {
    fn observe_start(&mut self, start: &[u8]) -> Result<Observation, String> {
        self.machine.restore_bytes(start).map_err(err)?;
        let wram = self.machine.read_wram().map_err(err)?;
        let cart = if self.game.cartridge_ram {
            self.machine.read_save_ram().map_err(err)?
        } else {
            Vec::new()
        };
        let mut scalars = Vec::new();
        self.game.scalars_into(&wram, &cart, &mut scalars);
        Ok(Observation {
            scalars,
            array: wram.to_vec(),
            milestone: false,
        })
    }

    fn run(
        &mut self,
        start: &[u8],
        actions: &[Action],
        probe: &mut dyn Probe,
    ) -> Result<u64, String> {
        let chords = self.chords(actions);
        self.stage(start, &chords)?;
        let mut work = 0u64;
        for (i, a) in actions.iter().enumerate() {
            for s in 0..a.hold {
                self.frame()?;
                work += 1;
                let milestone = self.milestone_holds();
                let mut scalars = std::mem::take(&mut self.scalars);
                let (wram, cart) = self.ram();
                self.game.scalars_into(wram, cart, &mut scalars);
                let want = probe.observe(
                    Position { action: i, step: s },
                    work,
                    &scalars,
                    wram,
                    milestone,
                );
                self.scalars = scalars;
                match want {
                    Want::Nothing => {}
                    Want::Capture => {
                        self.machine.capture_into(&mut self.capture).map_err(err)?;
                        probe.keep(self.capture.clone());
                    }
                    Want::Stop => return Ok(work),
                }
            }
        }
        Ok(work)
    }
}
