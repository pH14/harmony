// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::BTreeMap,
    env,
    error::Error,
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use nes_observatory::{
    contract::RunIdentity,
    producer::{Producer, export_spool, random_id},
    query::{self, MapFilter, Metric, ObservationFilter, TimelineFilter},
    server,
    store::Store,
};
use nes_workload::metroid::{
    archive::{MAX_ARCHIVE_ENTRIES, MAX_METROID_ACTIONS},
    campaign::{MetroidCampaignRun, MetroidGame},
};
use searcher::search::{
    archive::{RetentionPolicy, RetireThresholds, SelectorPolicy},
    campaign::{
        CampaignConfig, CampaignExecutionOptions, CampaignOrigin, InputPolicy, Reporting,
        run_campaign_checkpointed_with_observer,
    },
    draw::{DrawMixture, SuffixShape},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

fn flags(values: impl IntoIterator<Item = String>) -> Result<BTreeMap<String, String>> {
    let mut values = values.into_iter();
    let mut result = BTreeMap::new();
    while let Some(flag) = values.next() {
        if flag == "--json" || flag == "--no-telemetry" {
            result.insert(flag, "true".to_owned());
            continue;
        }
        if !flag.starts_with("--") {
            return Err(format!("expected a --flag, got {flag}").into());
        }
        let value = values
            .next()
            .ok_or_else(|| format!("missing value after {flag}"))?;
        if result.insert(flag.clone(), value).is_some() {
            return Err(format!("duplicate flag {flag}").into());
        }
    }
    Ok(result)
}

fn take(flags: &mut BTreeMap<String, String>, name: &str) -> Result<String> {
    flags
        .remove(name)
        .ok_or_else(|| format!("missing {name}").into())
}

fn optional(flags: &mut BTreeMap<String, String>, name: &str, default: &str) -> String {
    flags.remove(name).unwrap_or_else(|| default.to_owned())
}

fn number<T>(value: String, name: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error| format!("{name}: {error}").into())
}

fn take_number<T>(flags: &mut BTreeMap<String, String>, name: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    number(take(flags, name)?, name)
}

fn optional_number<T>(flags: &mut BTreeMap<String, String>, name: &str, default: T) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match flags.remove(name) {
        Some(value) => number(value, name),
        None => Ok(default),
    }
}

fn done(mut flags: BTreeMap<String, String>) -> Result<()> {
    flags.remove("--json");
    if let Some((name, _)) = flags.first_key_value() {
        return Err(format!("unexpected flag {name}").into());
    }
    Ok(())
}

fn store() -> Result<Store> {
    let url =
        env::var("HARMONY_CLICKHOUSE_URL").unwrap_or_else(|_| "http://127.0.0.1:8123".to_owned());
    let user = env::var("HARMONY_CLICKHOUSE_USER").unwrap_or_else(|_| "default".to_owned());
    let password = env::var("HARMONY_CLICKHOUSE_PASSWORD").unwrap_or_default();
    Store::new(url, user, password)
}

fn source_revision() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn run_metroid(mut flags: BTreeMap<String, String>) -> Result<Value> {
    let rom_path = PathBuf::from(take(&mut flags, "--rom")?);
    let core_path = PathBuf::from(take(&mut flags, "--core")?);
    let output = PathBuf::from(take(&mut flags, "--output")?);
    let spool = PathBuf::from(optional(
        &mut flags,
        "--spool",
        "/tmp/harmony-observatory-spool",
    ));
    let seed = optional_number(&mut flags, "--seed", 1_u64)?;
    let workers = optional_number(&mut flags, "--workers", 2_u32)?;
    let executions = optional_number(&mut flags, "--executions", 1000_u64)?;
    let actions = optional_number(&mut flags, "--actions", 4096_usize)?;
    let memory_mib = optional_number(&mut flags, "--memory-mib", 1024_usize)?;
    let disabled = flags.remove("--no-telemetry").is_some();
    done(flags)?;
    if workers == 0
        || workers > 64
        || executions == 0
        || actions == 0
        || actions > MAX_METROID_ACTIONS
        || memory_mib == 0
    {
        return Err("run limits are outside supported bounds".into());
    }
    if output.exists() && fs::read_dir(&output)?.next().is_some() {
        return Err("output directory must be empty".into());
    }
    fs::create_dir_all(&output)?;
    let rom = fs::read(&rom_path)?;
    let rom_sha256 = format!("{:x}", Sha256::digest(&rom));
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core_path)?));
    let game = MetroidGame::new(&rom, &core_path, &core_sha256)
        .with_milestone_input_dir(output.join("milestone-inputs"));
    let config = CampaignConfig {
        campaign_seed: seed,
        workers,
        execution_budget: executions,
        action_limit: actions,
        host: "metroid-observatory".to_owned(),
        wall_budget: None,
        stop_rollout_on_objective: true,
        stop_campaign_on_objective: false,
        archive_entry_limit: MAX_ARCHIVE_ENTRIES,
        reservations_per_worker: 1,
        memory_budget_mib: Some(memory_mib),
        materialize_final_artifacts: false,
        run: MetroidCampaignRun,
        suffix: SuffixShape::OneToSix,
        mixture: DrawMixture::AlphabetOnly,
        retention: RetentionPolicy::Unprobed,
        selector: SelectorPolicy::EnergyFrontierCheapest(RetireThresholds {
            entry: 3,
            groups: vec![6, 12, 2],
        }),
        objective_witness_path: Some(output.join("victory-input.json")),
    };
    let run_id = random_id()?;
    let session_id = random_id()?;
    let identity = RunIdentity {
        run_id: run_id.clone(),
        session_id: session_id.clone(),
        workload: "metroid-quicknes".to_owned(),
        build: source_revision(),
        rom_sha256,
        core_sha256,
        policy: serde_json::to_string(&game.policies(&MetroidCampaignRun))?,
        seed,
        workers,
        execution_budget: executions,
        action_limit: actions,
        work_unit: game.execution_work_unit().to_owned(),
        observation_boundary: "valid in-play action observations at ordered admission".to_owned(),
    };
    fs::write(
        output.join("telemetry-identity.json"),
        serde_json::to_vec_pretty(&identity)?,
    )?;
    let mut stream = BufWriter::new(fs::File::create(output.join("stream.jsonl"))?);
    let mut progress = BufWriter::new(fs::File::create(output.join("progress.jsonl"))?);
    let mut producer = if disabled {
        None
    } else {
        Some(Producer::start(identity, &spool, store()?)?)
    };
    let outcome = if let Some(producer) = producer.as_mut() {
        run_campaign_checkpointed_with_observer(
            &game,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            Some(&mut progress),
            CampaignExecutionOptions::default(),
            producer,
        )
    } else {
        run_campaign_checkpointed_with_observer(
            &game,
            &config,
            &CampaignOrigin::Genesis,
            &mut stream,
            Some(&mut progress),
            CampaignExecutionOptions::default(),
            &mut (),
        )
    };
    let status = if outcome.is_ok() {
        "search_complete"
    } else {
        "search_failed"
    };
    let losses = producer.take().map(|producer| producer.finish(status));
    stream.flush()?;
    progress.flush()?;
    let (report, _) = outcome.map_err(|error| error.to_string())?;
    fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(json!({
        "run_id":run_id,
        "session_id":session_id,
        "report":output.join("report.json"),
        "stream":output.join("stream.jsonl"),
        "spool":spool.join(&run_id).join(&session_id),
        "producer_queue_loss":losses.map(|loss| loss.0),
        "spool_loss":losses.map(|loss| loss.1),
        "executions":report.archive.executions,
        "execution_work":report.execution_work
    }))
}

fn query_command(command: &str, mut flags: BTreeMap<String, String>) -> Result<Value> {
    let store = store()?;
    let output = match command {
        "runs" => query::runs(&store)?,
        "status" => query::status(&store, &take(&mut flags, "--run")?)?,
        "map" => {
            let run_id = take(&mut flags, "--run")?;
            let metric: Metric = serde_json::from_value(json!(take(&mut flags, "--metric")?))?;
            let filter = MapFilter {
                metric,
                from_ms: take_number(&mut flags, "--from-ms")?,
                to_ms: take_number(&mut flags, "--to-ms")?,
                area: flags
                    .remove("--area")
                    .map(|value| number(value, "--area"))
                    .transpose()?,
                as_of_ms: flags
                    .remove("--as-of-ms")
                    .map(|value| number(value, "--as-of-ms"))
                    .transpose()?,
            };
            query::map(&store, &run_id, &filter)?
        }
        "timeline" => {
            let run_id = take(&mut flags, "--run")?;
            let filter = TimelineFilter {
                from_ms: take_number(&mut flags, "--from-ms")?,
                to_ms: take_number(&mut flags, "--to-ms")?,
                bucket_ms: optional_number(&mut flags, "--bucket-ms", 5000_u64)?,
            };
            query::timeline(&store, &run_id, &filter)?
        }
        "observations" => {
            let run_id = take(&mut flags, "--run")?;
            let filter = ObservationFilter {
                from_ms: take_number(&mut flags, "--from-ms")?,
                to_ms: take_number(&mut flags, "--to-ms")?,
                area: flags
                    .remove("--area")
                    .map(|value| number(value, "--area"))
                    .transpose()?,
                map_x: flags
                    .remove("--map-x")
                    .map(|value| number(value, "--map-x"))
                    .transpose()?,
                map_y: flags
                    .remove("--map-y")
                    .map(|value| number(value, "--map-y"))
                    .transpose()?,
                min_health: flags
                    .remove("--min-health")
                    .map(|value| number(value, "--min-health"))
                    .transpose()?,
                min_missiles: flags
                    .remove("--min-missiles")
                    .map(|value| number(value, "--min-missiles"))
                    .transpose()?,
                equipment_bits: flags
                    .remove("--equipment-bits")
                    .map(|value| number(value, "--equipment-bits"))
                    .transpose()?,
                outcome: flags.remove("--outcome"),
                after: flags.remove("--after"),
                limit: flags
                    .remove("--limit")
                    .map(|value| number(value, "--limit"))
                    .transpose()?,
            };
            query::observations(&store, &run_id, &filter)?
        }
        "draw-table" => {
            let run_id = take(&mut flags, "--run")?;
            let at_ms = take_number(&mut flags, "--at-ms")?;
            query::draw_table(&store, &run_id, at_ms)?
        }
        _ => return Err(format!("unknown command {command}").into()),
    };
    done(flags)?;
    Ok(output)
}

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let command = args.next().ok_or("expected run, bootstrap, serve, export, runs, status, map, timeline, observations, or draw-table")?;
    let mut flags = flags(args)?;
    if command == "serve" {
        let address = optional(&mut flags, "--bind", "127.0.0.1:8787");
        let static_dir = PathBuf::from(optional(
            &mut flags,
            "--static-dir",
            "website/observatory/dist",
        ));
        done(flags)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        runtime.block_on(server::serve(store()?, &address, static_dir))?;
        return Ok(());
    }
    let output = match command.as_str() {
        "run" => run_metroid(flags)?,
        "bootstrap" => {
            done(flags)?;
            store()?.bootstrap()?;
            json!({"ready":true})
        }
        "export" => {
            let directory = PathBuf::from(take(&mut flags, "--spool-dir")?);
            done(flags)?;
            let count = export_spool(Path::new(&directory), &store()?)?;
            json!({"uploaded_batches":count})
        }
        _ => query_command(&command, flags)?,
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
