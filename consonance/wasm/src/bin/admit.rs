// SPDX-License-Identifier: AGPL-3.0-or-later
use consonance_wasm::admission::{AdmittedModule, Profile};
use std::{env, fs, io::Read};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args_os().collect();
    if args.len() != 3 {
        return Err("usage: admit MODULE OUTPUT".into());
    }
    let profile = Profile::default();
    let mut bytes = Vec::new();
    fs::File::open(&args[1])?
        .take(u64::from(profile.maximum_module_bytes) + 1)
        .read_to_end(&mut bytes)?;
    let admitted = AdmittedModule::new(&bytes, profile)?;
    fs::write(&args[2], admitted.bytes())?;
    println!(
        "execution={:02x?}; source={:02x?}; bytes={}",
        admitted.execution_digest(),
        admitted.source_digest(),
        admitted.bytes().len()
    );
    Ok(())
}
