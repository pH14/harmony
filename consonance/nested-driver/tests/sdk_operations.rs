// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]

use consonance_client::{
    catalog::StateCatalog,
    session::{Session, SessionConfig},
};
use oci_support::{
    bundle::{self, LaunchRequest},
    image,
};
use std::{error::Error, time::Duration};

fn observation(session: &mut Session) -> Result<Vec<u64>, Box<dyn Error>> {
    let mut catalog = StateCatalog::default();
    for (_, id, bytes) in session.sdk_events()? {
        catalog.observe(id, &bytes)?;
    }
    [
        "creations",
        "imports",
        "steps",
        "bytes.0",
        "bytes.1",
        "bytes.2",
        "snapshots",
        "fork_depth",
        "operations",
        "operation",
        "pairs",
    ]
    .into_iter()
    .map(|name| catalog.get(&format!("nested.{name}")).map_err(Into::into))
    .collect()
}

fn advance(session: &mut Session, seed: u64) -> Result<Vec<u64>, Box<dyn Error>> {
    let (cut, at) = session.snapshot()?;
    session.branch_with_seed(cut, seed)?;
    session.run_to_snapshot(at)?;
    let observed = observation(session)?;
    session.drop_snapshot(cut)?;
    println!("NESTED_SDK_SMOKE seed={seed} registers={observed:?}");
    assert_eq!(observed[0], 1);
    Ok(observed)
}

#[test]
#[ignore = "requires nested VMX or SVM and matching nested-host OCI artifacts"]
fn outer_operation_sdk_smoke() -> Result<(), Box<dyn Error>> {
    let read =
        |name| -> Result<Vec<u8>, Box<dyn Error>> { Ok(std::fs::read(std::env::var(name)?)?) };
    let kernel = read("NESTED_HOST_KERNEL")?;
    let base = read("NESTED_OCI_INITRAMFS")?;
    let stage = tempfile::tempdir()?;
    let image = image::stage(&std::env::var("NESTED_DRIVER_IMAGE")?, stage.path())?;
    let request = LaunchRequest::new(vec![
        "/app/nested-driver".into(),
        "--sdk".into(),
        "--search".into(),
    ])
    .with_kvm();
    let initramfs = bundle::prepare(&image, &request)?.initramfs(&base);
    let mut config = SessionConfig {
        ram_bytes: 512 << 20,
        seed: 42,
        ..SessionConfig::default()
    }
    .with_nested_host()
    .with_deferred_virtual_time_checkpoint_hashes()
    .with_wall_limit(Duration::from_secs(15));
    config.cmdline.push_str(" kvm_intel.dump_invalid_vmcs=1");
    let mut session = Session::new_with_config(&kernel, &initramfs, config)?;
    assert_eq!(observation(&mut session)?[0..2], [1, 0]);
    let root = session.setup_sparse_snapshot()?;
    for _ in 0..2 {
        let imported = session.import_sparse_snapshot(&root)?;
        session.replay_snapshot(imported)?;
        assert_eq!(observation(&mut session)?[0..2], [1, 0]);
        session.drop_snapshot(imported)?;
    }
    let cold = advance(&mut session, 6_012_046_879_400_776_456)?;
    assert_eq!(cold[8..10], [1, 0]);
    for seed in [
        4_043_733_305_989_114_339,
        17_338_899_251_130_015_454,
        946_952_341_274_287_066,
        10_155_054_555_646_050_472,
    ] {
        advance(&mut session, seed)?;
    }
    for seed in 0..8 {
        advance(&mut session, seed)?;
    }
    let (cut, _) = session.snapshot()?;
    let captured = observation(&mut session)?;
    let expected = [advance(&mut session, 8)?, advance(&mut session, 9)?];
    advance(&mut session, 19)?;
    advance(&mut session, 20)?;
    session.replay_snapshot(cut)?;
    assert_eq!(
        observation(&mut session)?,
        captured,
        "outer restore changed the inner creation/import counts or published state"
    );
    let restored = [advance(&mut session, 8)?, advance(&mut session, 9)?];
    assert_eq!(
        restored, expected,
        "outer restored SDK operation continuation diverged"
    );
    session.drop_snapshot(cut)?;
    println!(
        "NESTED_SDK_SMOKE_OK creations=1 operations={} pairs={:x}",
        restored[1][8], restored[1][10]
    );
    Ok(())
}
