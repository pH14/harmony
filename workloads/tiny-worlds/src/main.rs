// SPDX-License-Identifier: AGPL-3.0-or-later

use searcher::search::draw::draw_mixture_from_identifier;
use serde::Deserialize;
use std::{error::Error, io::Read};
use tiny_worlds::{
    Hand, Keep, KeyKind, Scale, SearchSettings, Strategic, Workload, run_kept, run_scaled,
    worlds::World,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    config: World,
    seed: u64,
    work_budget: u64,
    broken: bool,
    verify: bool,
    keep: Keep,
    #[serde(default)]
    key: KeyKind,
    #[serde(default)]
    strategy: Option<String>,
    #[serde(default)]
    scale: Option<Scale>,
    #[serde(default)]
    workers: Option<u32>,
    #[serde(default)]
    search: Option<SearchRequest>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchRequest {
    #[serde(default)]
    mixture: Option<String>,
    #[serde(default)]
    stop_on_objective: Option<bool>,
}
fn main() -> Result<(), Box<dyn Error>> {
    let mut input = String::new();
    std::io::stdin().take(16_385).read_to_string(&mut input)?;
    if input.len() > 16_384 {
        return Err("request exceeds 16KB".into());
    }
    let request: Request = serde_json::from_str(&input)?;
    if request.strategy.is_some() && request.key != KeyKind::Strategy {
        return Err("a strategy text needs key strategy".into());
    }
    let mut settings = SearchSettings::default();
    if let Some(search) = &request.search {
        settings.mixture = search
            .mixture
            .as_deref()
            .map(draw_mixture_from_identifier)
            .transpose()?;
        settings.stop_on_objective = search.stop_on_objective;
    }
    let workload = Workload {
        keying: Hand::<false>,
        config: request.config,
        broken: request.broken,
        scale: request.scale,
    };
    workload.config.validate()?;
    if !workload.config.reachable()? {
        return Err("world objective is unreachable".into());
    }
    if request.scale.is_some() {
        if request.workers.is_some() {
            return Err("scaled runs set workers in scale".into());
        }
        if request.verify || request.keep != Keep::Portfolio {
            return Err("scaled runs need verify false and keep portfolio".into());
        }
        let mut progress = std::io::LineWriter::new(std::io::stderr().lock());
        let report = match request.key {
            KeyKind::Hand => run_scaled(
                &workload,
                request.seed,
                request.work_budget,
                &mut progress,
                settings,
            )?,
            KeyKind::Strategy => {
                let keying = match request.strategy.as_deref() {
                    Some(text) => Strategic::new(&workload.config, workload.broken, text)?,
                    None => Strategic::shipped(&workload.config, workload.broken, request.keep)?,
                };
                run_scaled(
                    &Workload {
                        config: workload.config.clone(),
                        broken: workload.broken,
                        scale: workload.scale,
                        keying,
                    },
                    request.seed,
                    request.work_budget,
                    &mut progress,
                    settings,
                )?
            }
        };
        println!("{report}");
        return Ok(());
    }
    println!(
        "{}",
        run_kept(
            &workload,
            request.keep,
            request.key,
            request.strategy.as_deref(),
            request.seed,
            request.work_budget,
            request.verify,
            request.workers.unwrap_or(1),
            settings
        )?
    );
    Ok(())
}
