// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end Linux/KVM acceptance for the replicated-service workload.

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
fn main() {
    if let Err(error) = live::run() {
        eprintln!("FAULT_VM_ORACLE_FAIL: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
)))]
fn main() {
    eprintln!("fault-vm-oracle requires Linux x86_64/aarch64 and the consonance feature");
    std::process::exit(2);
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
mod live {
    use std::{error::Error, fs, path::PathBuf};

    use faults_workload::{
        CatalogAction, FaultAction, SearchOptions, game::FaultGame, prepare::prepare_oci,
        search_consonance,
    };

    pub fn run() -> Result<(), Box<dyn Error>> {
        let args: Vec<_> = std::env::args().collect();
        let image = required_path(&args, "--oci")?;
        let kernel = required_path(&args, "--kernel")?;
        let base = required_path(&args, "--base-initramfs")?;
        let agent = required_path(&args, "--fault-agent")?;
        let output = required_path(&args, "--out")?;

        let kernel_bytes = fs::read(&kernel)?;
        let prepared = prepare_oci(
            image.to_str().ok_or("OCI path is not UTF-8")?,
            &fs::read(&base)?,
            &fs::read(&agent)?,
        )?;
        let game = FaultGame::new_consonance(&prepared.spec, &kernel_bytes, &prepared.initramfs)?;
        let mut target = game.acceptance_target()?;
        let origin = target.snapshot();

        let nominal = FaultAction::new(CatalogAction::Advance, 0, 0);
        target.apply_action(nominal)?;
        if target.observation().assertion != 0 || target.observation().execution_error {
            return Err(format!("nominal action failed: {:?}", target.observation()).into());
        }

        let first_hash = target.state_hash()?;
        let first_observation = target.observation().clone();
        target.restore(&origin)?;
        target.apply_action(nominal)?;
        let second_hash = target.state_hash()?;
        if first_hash != second_hash || first_observation != *target.observation() {
            return Err("restoring the origin and replaying nominal action diverged".into());
        }

        let partition = FaultAction::new(CatalogAction::PartitionForward, 0, 0);
        target.restore(&origin)?;
        target.apply_action(partition)?;
        if target.observation().assertion == 0 || !target.observation().partitioned {
            return Err(format!(
                "directional partition did not expose stale read: {:?}",
                target.observation()
            )
            .into());
        }

        let partition_hash = target.state_hash()?;
        let partition_observation = target.observation().clone();
        target.restore(&origin)?;
        target.apply_action(partition)?;
        if partition_hash != target.state_hash()? || partition_observation != *target.observation()
        {
            return Err("restoring the origin and replaying partition action diverged".into());
        }

        target.apply_action(FaultAction::new(CatalogAction::RecoverForward, 0, 0))?;
        if target.observation().assertion != 0 || target.observation().execution_error {
            return Err(format!("recovery did not converge: {:?}", target.observation()).into());
        }

        // Keep a real directional delay active across a whole-VM snapshot,
        // then prove both replay and the recovery continuation.  The delay is
        // distinct from a partition: the fixture acknowledges a queued,
        // unapplied replication request while the qdisc remains installed;
        // only RecoverForward's delivery check clears it.
        let delay = FaultAction::new(CatalogAction::DelayForward, 0, 0);
        target.restore(&origin)?;
        target.apply_action(delay)?;
        if target.observation().assertion != 0
            || target.observation().execution_error
            || target.observation().partitioned
            || !target.observation().delayed
            || !target.observation().pending_work
        {
            return Err(format!(
                "delay action did not preserve a healthy delayed state: {:?}",
                target.observation()
            )
            .into());
        }
        let delayed_snapshot = target.snapshot();
        let delayed_hash = target.state_hash()?;
        let delayed_observation = target.observation().clone();

        target.restore(&origin)?;
        target.apply_action(delay)?;
        if delayed_hash != target.state_hash()?
            || delayed_observation != *target.observation()
            || delayed_snapshot != target.snapshot()
        {
            return Err("restoring the origin and replaying delay diverged".into());
        }

        target.restore(&delayed_snapshot)?;
        target.apply_action(FaultAction::new(CatalogAction::RecoverForward, 0, 0))?;
        if target.observation().assertion != 0
            || target.observation().execution_error
            || target.observation().delayed
            || target.observation().pending_work
        {
            return Err(format!(
                "delay recovery did not converge: {:?}",
                target.observation()
            )
            .into());
        }
        let delay_recovery_hash = target.state_hash()?;
        let delay_recovery_observation = target.observation().clone();

        target.restore(&delayed_snapshot)?;
        target.apply_action(FaultAction::new(CatalogAction::RecoverForward, 0, 0))?;
        if delay_recovery_hash != target.state_hash()?
            || delay_recovery_observation != *target.observation()
        {
            return Err("restoring an active delay and replaying recovery diverged".into());
        }

        // Restart is a separate pending-process branch.  The guest performs a
        // bounded readiness probe after replacing the replica; readiness or
        // control failures are published as execution_error, never as the
        // stale-read assertion.  Compare both the restart stop and a suffix
        // continuation restored from that stop.
        let restart = FaultAction::new(CatalogAction::RestartReplica, 0, 0);
        target.restore(&origin)?;
        target.apply_action(restart)?;
        if target.observation().execution_error || target.observation().assertion != 0 {
            return Err(format!(
                "replica restart did not recover cleanly: {:?}",
                target.observation()
            )
            .into());
        }
        let restart_snapshot = target.snapshot();
        let restart_hash = target.state_hash()?;
        let restart_observation = target.observation().clone();

        target.restore(&origin)?;
        target.apply_action(restart)?;
        if restart_hash != target.state_hash()?
            || restart_observation != *target.observation()
            || restart_snapshot != target.snapshot()
        {
            return Err("restoring the origin and replaying restart diverged".into());
        }

        let continuation = FaultAction::new(CatalogAction::Advance, 0, 0);
        target.restore(&restart_snapshot)?;
        target.apply_action(continuation)?;
        let restart_continuation_hash = target.state_hash()?;
        let restart_continuation_observation = target.observation().clone();
        target.restore(&restart_snapshot)?;
        target.apply_action(continuation)?;
        if restart_continuation_hash != target.state_hash()?
            || restart_continuation_observation != *target.observation()
        {
            return Err("restoring a restart stop and replaying its continuation diverged".into());
        }

        // The pause interval yields guest time while the replica is stopped;
        // invariant checks run after resume, when its control endpoint can reply.
        let pause = FaultAction::new(CatalogAction::PauseReplica, 2, 0);
        target.restore(&origin)?;
        target.apply_action(pause)?;
        if target.observation().execution_error || target.observation().assertion != 0 {
            return Err(format!(
                "paused replica did not resume cleanly: {:?}",
                target.observation()
            )
            .into());
        }
        let pause_hash = target.state_hash()?;
        let pause_observation = target.observation().clone();
        target.restore(&origin)?;
        target.apply_action(pause)?;
        if pause_hash != target.state_hash()? || pause_observation != *target.observation() {
            return Err("restoring the origin and replaying pause/resume diverged".into());
        }

        let options = SearchOptions {
            seed: 0xFA_u64,
            workers: 1,
            executions: 64,
            actions: 4,
            output: output.clone(),
        };
        search_consonance(&prepared.spec, &kernel_bytes, &prepared.initramfs, &options)?;
        let victory = output.join("victory.json");
        if !victory.is_file() || fs::metadata(&victory)?.len() == 0 {
            return Err("bounded Dissonance campaign did not materialize a victory".into());
        }

        println!(
            "FAULT_VM_ORACLE_OK nominal=1 partition_assertion=1 restore_equal=1 delay_restore=1 delay_pending=1 restart_restore=1 pause_restore=1 recovery=1 search_victory=1"
        );
        Ok(())
    }

    fn required_path(args: &[String], flag: &str) -> Result<PathBuf, Box<dyn Error>> {
        args.windows(2)
            .find(|pair| pair[0] == flag)
            .map(|pair| PathBuf::from(&pair[1]))
            .ok_or_else(|| format!("missing {flag}").into())
    }
}
