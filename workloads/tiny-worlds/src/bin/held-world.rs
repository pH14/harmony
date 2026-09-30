// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, io::Read};
use tiny_worlds::held::{Request, run};

fn main() -> Result<(), Box<dyn Error>> {
    let mut input = String::new();
    std::io::stdin().take(16_385).read_to_string(&mut input)?;
    if input.len() > 16_384 {
        return Err("request exceeds 16KB".into());
    }
    let request: Request = serde_json::from_str(&input)?;
    println!("{}", run(&request)?);
    Ok(())
}
