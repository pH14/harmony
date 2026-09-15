// SPDX-License-Identifier: AGPL-3.0-or-later

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let session = consonance_client::session::SessionConfig::default();
    let session_json = serde_json::to_vec(&session).expect("serialize default session");
    if let Err(error) = nes_workload::admission::dump(&args, &session_json, None) {
        eprintln!("prepare-admission: {error}");
        std::process::exit(1);
    }
}
