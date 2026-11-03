// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::nes::ButtonChord;

pub const SETTLE_FRAMES: u64 = 2000;

fn idle(frames: u32, chords: &mut Vec<ButtonChord>) {
    let mut left = frames;
    while left > 0 {
        let step = left.min(120);
        chords.push(ButtonChord::new(0, step as u8));
        left -= step;
    }
}

pub fn tape(game: &str, level: u8) -> Result<Vec<ButtonChord>, String> {
    let mut c = Vec::new();
    match game {
        "smb" => {
            idle(120, &mut c);
            c.push(ButtonChord::new(8, 1));
        }
        "famidash" => {
            idle(22, &mut c);
            c.push(ButtonChord::new(1, 1));
            idle(936, &mut c);
            c.push(ButtonChord::new(1, 1));
            idle(110, &mut c);
            for _ in 0..level {
                c.push(ButtonChord::new(0x80, 1));
                idle(20, &mut c);
            }
            c.push(ButtonChord::new(1, 1));
            idle(40, &mut c);
        }
        "zelda" => {
            let press = |c: &mut Vec<ButtonChord>, b: u8| {
                c.push(ButtonChord::new(b, 2));
                idle(30, c);
            };
            idle(120, &mut c);
            press(&mut c, 8);
            idle(60, &mut c);
            press(&mut c, 8);
            idle(60, &mut c);
            press(&mut c, 1);
            for _ in 0..3 {
                press(&mut c, 4);
            }
            press(&mut c, 8);
            idle(60, &mut c);
            press(&mut c, 8);
        }
        "ice-climbers" => {
            idle(120, &mut c);
            c.push(ButtonChord::new(8, 2));
            idle(780, &mut c);
        }
        "metroid" => {
            idle(300, &mut c);
            for _ in 0..2 {
                c.push(ButtonChord::new(8, 4));
                idle(180, &mut c);
            }
        }
        other => return Err(format!("no boot tape for {other}")),
    }
    Ok(c)
}

pub fn playing(game: &str, level: u8, wram: &[u8]) -> bool {
    match game {
        "smb" => wram[0x770] == 1 && wram[0x772] == 3,
        "famidash" => wram[0x498] == 2 && wram[0x47b] == level && wram[0x5b5] == 0,
        "metroid" => wram[0x1e] == 3,
        "zelda" => wram[0x12] == 5,
        "ice-climbers" => {
            wram[0x59] == 0
                && wram[0x20] == 3
                && wram[0x64] == 0x38
                && wram[0x66] == 0xc0
                && wram[0x13] == 0x9f
        }
        _ => false,
    }
}
