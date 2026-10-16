// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::PathBuf;
use std::time::Duration;

use consonance_client::session::{self, SessionConfig};
use uml::{Bridge, Guest, Launch, Profile, Recording};

#[link(name = "witness")]
unsafe extern "C" {
    fn witness_anchor() -> u32;
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().ok_or("missing mode")?;
    let profile_path = PathBuf::from(args.next().ok_or("missing profile")?);
    let work_path = PathBuf::from(args.next().ok_or("missing work directory")?);
    let recording_path = PathBuf::from(args.next().ok_or("missing recording path")?);
    // SAFETY: the linked witness library exports this no-argument function with the declared ABI.
    if unsafe { witness_anchor() } != 0x688 {
        return Err("ordinary witness dependency was not linked".into());
    }
    let selectors: Vec<_> = std::env::vars_os()
        .filter_map(|(name, _)| {
            let name = name.to_string_lossy();
            (name.starts_with("LD_") || name == "GLIBC_TUNABLES").then(|| name.into_owned())
        })
        .collect();
    if !selectors.is_empty() {
        return Err(format!("loader selectors present: {selectors:?}").into());
    }
    let session_identity = session::identity_with_config_and_payloads(
        b"kernel",
        b"initramfs",
        &SessionConfig::default(),
        &[],
    )
    .map_err(std::io::Error::other)?;
    if profile_path.as_os_str() == "-" {
        let host_executable_identity =
            host_code_identity::loaded_executable_identity("loaded-code-witness-main-v1")?;
        if mode == "check" {
            let prior: serde_json::Value = serde_json::from_slice(&std::fs::read(recording_path)?)?;
            let prior_host = prior["host_executable_identity"]
                .as_str()
                .ok_or("missing prior host identity")?;
            let prior_session = prior["session_identity"]
                .as_str()
                .ok_or("missing prior session identity")?;
            if prior_host != host_executable_identity || prior_session != session_identity {
                return Err(format!(
                    "loaded-code witness identity mismatch: host {prior_host} -> {host_executable_identity}; session {prior_session} -> {session_identity}"
                )
                .into());
            }
            println!("admitted");
            return Ok(());
        }
        if mode != "record" {
            return Err("mode must be record or check".into());
        }
        let result = serde_json::json!({
            "host_executable_identity": host_executable_identity,
            "session_identity": session_identity,
            "selectors": selectors,
        });
        std::fs::write(&recording_path, serde_json::to_vec_pretty(&result)?)?;
        println!("{result}");
        return Ok(());
    }
    let profile = Profile::load(&profile_path)?;
    if mode == "check" {
        let prior: Recording = serde_json::from_slice(&std::fs::read(recording_path)?)?;
        prior.check(&profile)?;
        println!("admitted");
        return Ok(());
    }
    if mode != "record" {
        return Err("mode must be record or check".into());
    }
    let mut launch = Launch::new(work_path);
    launch.bridge = Some(Bridge::new(0x5eed_0001));
    launch.kernel_arguments = vec!["harmony_fixture=values".to_owned()];
    launch.wall_limit = Duration::from_secs(60);
    let events = if std::env::var_os("HARMONY_LOADED_CODE_RUN_GUEST").is_some() {
        Guest::spawn(&launch, &profile)?.wait()?.events
    } else {
        Vec::new()
    };
    let recording = Recording::new(&profile, &launch, &events)?;
    let entropy = events
        .iter()
        .find(|event| {
            event
                .data
                .windows(b"harmony_entropy".len())
                .any(|part| part == b"harmony_entropy")
        })
        .map(|event| String::from_utf8_lossy(&event.data).into_owned());
    std::fs::write(&recording_path, serde_json::to_vec_pretty(&recording)?)?;
    println!(
        "{}",
        serde_json::json!({
            "host_executable_identity": recording.host_executable_identity,
            "session_identity": session_identity,
            "profile_identity_sha256": recording.profile_identity_sha256,
            "event_hash": recording.event_hash,
            "events": recording.events,
            "harmony_entropy": entropy,
            "selectors": selectors,
        })
    );
    Ok(())
}
