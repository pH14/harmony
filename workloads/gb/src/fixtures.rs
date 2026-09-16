// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use machine::gb::ButtonChord;

use crate::target::BlueAction;

pub const SETUP_CHORDS_JSON: &str = include_str!("../fixtures/setup.json");
pub const BROCK_ROUTE_JSON: &str = include_str!("../fixtures/brock.json");

pub fn setup_prefix() -> Result<Vec<ButtonChord>, Box<dyn Error>> {
    Ok(serde_json::from_str(SETUP_CHORDS_JSON)?)
}

pub fn brock_route() -> Result<Vec<BlueAction>, Box<dyn Error>> {
    Ok(serde_json::from_str(BROCK_ROUTE_JSON)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_fixtures_parse_and_stay_within_the_action_limit() {
        let prefix = setup_prefix().unwrap();
        assert!(!prefix.is_empty());
        assert!(prefix.iter().all(|chord| chord.hold_frames > 0));
        let route = brock_route().unwrap();
        assert!(!route.is_empty());
        assert!(route.len() <= crate::archive::MAX_BLUE_ACTIONS);
    }
}
