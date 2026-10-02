// SPDX-License-Identifier: AGPL-3.0-or-later

use consonance_wasm::{
    Invocation, WasmSession,
    admission::{AdmittedModule, Profile},
};
use environment::input_spec::{InputSpec, nominal_factory};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err("usage: debug-map SOURCE.wasm OUTPUT.json".into());
    }
    let admitted = AdmittedModule::new(&std::fs::read(&args[1])?, Profile::default())?;
    let session = WasmSession::new(
        admitted,
        InputSpec::seeded(0),
        Invocation::new("play"),
        nominal_factory(),
    )?;
    let map = session.debug_map();
    for function in &map.functions {
        let compiled = map
            .compiled
            .iter()
            .find(|compiled| compiled.function == function.function)
            .ok_or("missing compiled function mapping")?;
        if compiled.locations.is_empty()
            || compiled
                .locations
                .iter()
                .any(|location| location.source.is_none())
        {
            return Err(format!(
                "incomplete source mapping for function {}",
                function.function
            )
            .into());
        }
    }
    std::fs::write(&args[2], serde_json::to_vec(&map)?)?;
    Ok(())
}
