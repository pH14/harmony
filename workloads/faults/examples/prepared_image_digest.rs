// SPDX-License-Identifier: AGPL-3.0-or-later

//! Print the digest of the initramfs produced by the normal image preparer.
//!
//! This helper deliberately stops after preparation: it does not boot a guest
//! or execute any workload command.

use std::{env, error::Error, fs};

use faults_workload::prepare::prepare_oci;
use sha2::{Digest, Sha256};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let image = args
        .next()
        .ok_or("usage: prepared_image_digest IMAGE BASE AGENT")?;
    let base_path = args
        .next()
        .ok_or("usage: prepared_image_digest IMAGE BASE AGENT")?;
    let agent_path = args
        .next()
        .ok_or("usage: prepared_image_digest IMAGE BASE AGENT")?;
    if args.next().is_some() {
        return Err("usage: prepared_image_digest IMAGE BASE AGENT".into());
    }

    let base = fs::read(base_path)?;
    let agent = fs::read(agent_path)?;
    let prepared = prepare_oci(&image, &base, &agent)?;
    println!("{:x}", Sha256::digest(&prepared.initramfs));
    Ok(())
}
