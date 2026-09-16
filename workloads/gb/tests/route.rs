// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use blue_workload::{
    archive::archive_key,
    fixtures::{brock_route, setup_prefix},
    target::BlueTarget,
};
use searcher::target::{ExitKind, Target};
use sha2::{Digest, Sha256};

fn core_and_rom() -> Result<(PathBuf, String, Vec<u8>), Box<dyn Error>> {
    let core = PathBuf::from(
        env::var_os("HARMONY_GAMBATTE_CORE")
            .ok_or("HARMONY_GAMBATTE_CORE must name the pinned libretro core")?,
    );
    let identity = format!("{:x}", Sha256::digest(fs::read(&core)?));
    let rom = fs::read(PathBuf::from(
        env::var_os("HARMONY_BLUE_ROM")
            .ok_or("HARMONY_BLUE_ROM must name the external Pokemon Blue ROM")?,
    ))?;
    Ok((core, identity, rom))
}

#[test]
#[ignore = "needs the pinned Gambatte core and the Pokemon Blue ROM"]
fn the_scripted_route_reaches_the_boulder_badge() {
    let (core, identity, rom) = core_and_rom().expect("core and ROM");
    let prefix = setup_prefix().expect("setup fixture");
    let mut target =
        BlueTarget::from_rom_bytes_after(&rom, &core, &identity, &prefix).expect("target");
    let start = target.state();
    assert_eq!(start.party_count, 0);
    assert!(!start.has_badge());

    for action in &brock_route().expect("route fixture") {
        target.apply(action);
        assert_eq!(target.exit_kind(), ExitKind::Ok);
    }

    let end = target.state();
    assert!(
        end.has_badge(),
        "the route ended without the badge: {end:?}"
    );
    assert!(target.is_victory());
    assert!(!target.is_dead());
    assert_eq!(end.milestone_flags(), 0xff);
    assert_eq!(archive_key(end).badges, end.badges);
}

#[test]
#[ignore = "needs the pinned Gambatte core and the Pokemon Blue ROM"]
fn replaying_the_route_twice_lands_on_the_same_state() {
    let (core, identity, rom) = core_and_rom().expect("core and ROM");
    let prefix = setup_prefix().expect("setup fixture");
    let route = brock_route().expect("route fixture");
    let mut target =
        BlueTarget::from_rom_bytes_after(&rom, &core, &identity, &prefix).expect("target");
    let mut endpoints = Vec::new();
    let mut spent = Vec::new();
    for _ in 0..2 {
        let before = target.execution_work();
        target.reset();
        for action in &route {
            target.apply(action);
        }
        endpoints.push(target.state());
        spent.push(target.execution_work() - before);
    }
    assert_eq!(endpoints[0], endpoints[1]);
    assert_eq!(spent[0], spent[1]);
    assert!(endpoints[0].has_badge());
}
