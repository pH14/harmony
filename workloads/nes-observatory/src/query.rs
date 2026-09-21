// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    contract::SCHEMA_VERSION,
    store::{Result, Store},
};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    Selections,
    Work,
    Positions,
    NewPlaces,
    Territory,
}

impl Metric {
    fn kind(&self) -> &'static str {
        match self {
            Self::Selections => "kind IN ('selection','skip')",
            Self::Work => "kind='work'",
            Self::Positions => "kind='presence'",
            Self::NewPlaces | Self::Territory => "kind='discovery'",
        }
    }

    fn sum(&self) -> &'static str {
        match self {
            Self::Work => "sum(execution_work)",
            Self::Positions => "sum(amount)",
            Self::Selections | Self::NewPlaces | Self::Territory => "sum(amount)",
        }
    }

    pub fn unit(&self) -> &'static str {
        match self {
            Self::Selections => "selections",
            Self::Work => "emulated_frames",
            Self::Positions => "gameplay_observations",
            Self::NewPlaces | Self::Territory => "map_cells",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Selections => "selections",
            Self::Work => "work",
            Self::Positions => "positions",
            Self::NewPlaces => "new_places",
            Self::Territory => "territory",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct MapFilter {
    pub metric: Metric,
    pub from_ms: u64,
    pub to_ms: u64,
    pub area: Option<u8>,
    pub as_of_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ObservationFilter {
    pub from_ms: u64,
    pub to_ms: u64,
    pub area: Option<u8>,
    pub map_x: Option<u8>,
    pub map_y: Option<u8>,
    pub min_health: Option<u16>,
    pub min_missiles: Option<u8>,
    pub equipment_bits: Option<u8>,
    pub outcome: Option<String>,
    pub after: Option<String>,
    pub limit: Option<u16>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TimelineFilter {
    pub from_ms: u64,
    pub to_ms: u64,
    pub bucket_ms: u64,
}

fn run(run_id: &str) -> Result<&str> {
    if run_id.len() != 32 || !run_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("run ID must be 32 hexadecimal characters".into());
    }
    Ok(run_id)
}

fn window(from: u64, to: u64, maximum: u64) -> Result<()> {
    if from >= to || to.saturating_sub(from) > maximum {
        return Err("time interval is empty or exceeds the query span cap".into());
    }
    Ok(())
}

fn data(value: &Value) -> Value {
    value.get("data").cloned().unwrap_or_else(|| json!([]))
}

fn statistics(value: &Value) -> Value {
    value
        .get("statistics")
        .cloned()
        .unwrap_or_else(|| json!({}))
}

pub fn runs(store: &Store) -> Result<Value> {
    let result = store.query(
        "SELECT run_id, min(started_unix_ms) AS started_unix_ms, max(event_ms) AS latest_ms, \
         countIf(kind='selection' OR kind='skip') AS selections, \
         countIf(kind='work') AS admissions, countIf(kind='run_end') AS ended, \
         sumIf(amount,kind='loss') AS lost_events \
         FROM observatory.events FINAL \
         WHERE kind IN ('run_start','run_end','selection','skip','work','loss') \
         GROUP BY run_id ORDER BY started_unix_ms DESC LIMIT 100",
    )?;
    Ok(json!({"schema_version":SCHEMA_VERSION,"runs":data(&result),"cost":statistics(&result)}))
}

pub fn status(store: &Store, run_id: &str) -> Result<Value> {
    let run_id = run(run_id)?;
    let result = store.query(&format!(
        "SELECT min(started_unix_ms) AS started_unix_ms, max(event_ms) AS latest_ms, \
         count() AS retained_events, countIf(kind='run_start') AS started, countIf(kind='run_end') AS ended, \
         countIf(kind='run_end' AND payload='search_complete') AS completed, \
         argMaxIf(payload,event_id,kind='run_start') AS identity, \
         maxIf(selection_ms,kind='watermark') AS finalized_watermark_ms, \
         countIf(kind='selection' OR kind='skip') AS selections, \
         countIf(kind='skip') AS skipped, countIf(kind='work') AS admissions, \
         sumIf(execution_work,kind='work') AS execution_work, \
         sumIf(amount,kind='loss') AS lost_events, \
         max(event_id)-min(event_id)+1-count() AS sequence_gaps \
         FROM observatory.events FINAL WHERE run_id='{run_id}'"
    ))?;
    let row = result
        .get("data")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let started = row.get("started").and_then(Value::as_u64).unwrap_or(0) > 0;
    let ended = row.get("ended").and_then(Value::as_u64).unwrap_or(0) > 0;
    let completed = row.get("completed").and_then(Value::as_u64).unwrap_or(0) > 0;
    if row
        .get("retained_events")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        == 0
    {
        return Err("run has no retained telemetry".into());
    }
    let losses = row.get("lost_events").and_then(Value::as_u64).unwrap_or(0);
    let gaps = row
        .get("sequence_gaps")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let latest = row.get("latest_ms").and_then(Value::as_u64).unwrap_or(0);
    let watermark = row
        .get("finalized_watermark_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let identity = row
        .get("identity")
        .and_then(Value::as_str)
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    let mut telemetry = row;
    if let Some(fields) = telemetry.as_object_mut() {
        fields.remove("identity");
    }
    Ok(json!({
        "schema_version":SCHEMA_VERSION,
        "run_id":run_id,
        "identity":identity,
        "status":if completed {"completed"} else if ended {"failed"} else {"running_or_interrupted"},
        "telemetry":telemetry,
        "freshness_watermark_ms":latest,
        "finalized_through_ms":if ended {latest} else {watermark},
        "completeness":{
            "complete":started && losses==0 && gaps==0 && completed,
            "lost_events":losses,
            "sequence_gaps":gaps,
            "qualification":"All spatial totals are exact for received events. Missing events or an unended run make them incomplete."
        },
        "cost":statistics(&result)
    }))
}

fn territory(
    store: &Store,
    run_id: &str,
    to_ms: u64,
    area: Option<u8>,
) -> Result<(Value, Value, Option<u64>)> {
    let checkpoint = store.query(&format!(
        "SELECT event_ms,payload FROM observatory.events FINAL \
         WHERE run_id='{run_id}' AND kind='territory_checkpoint' AND event_ms < {to_ms} \
         ORDER BY event_ms DESC,event_id DESC LIMIT 1"
    ))?;
    let row = checkpoint
        .get("data")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first());
    let checkpoint_ms = row
        .and_then(|row| row.get("event_ms"))
        .and_then(Value::as_u64);
    let mut cells = BTreeSet::<(u8, u8, u8)>::new();
    if let Some(payload) = row
        .and_then(|row| row.get("payload"))
        .and_then(Value::as_str)
    {
        if payload.len() != 1280 {
            return Err("territory checkpoint has an invalid length".into());
        }
        for region in 0..5_usize {
            let area_byte = 16 + region as u8;
            if area.is_some_and(|wanted| wanted != area_byte) {
                continue;
            }
            for byte_index in 0..128_usize {
                let offset = region * 256 + byte_index * 2;
                let byte = u8::from_str_radix(&payload[offset..offset + 2], 16)?;
                for bit in 0..8_usize {
                    if byte & (1 << bit) != 0 {
                        let index = byte_index * 8 + bit;
                        cells.insert((area_byte, (index % 32) as u8, (index / 32) as u8));
                    }
                }
            }
        }
    }
    let since = checkpoint_ms.unwrap_or(0);
    let area_clause = area.map_or(String::new(), |value| format!(" AND area={value}"));
    let delta = store.query(&format!(
        "SELECT area,map_x,map_y FROM observatory.events FINAL \
         WHERE run_id='{run_id}' AND kind='discovery' \
         AND event_ms >= {since} AND event_ms < {to_ms}{area_clause} \
         GROUP BY area,map_x,map_y LIMIT 5120"
    ))?;
    if let Some(rows) = delta.get("data").and_then(Value::as_array) {
        for row in rows {
            let region = row["area"].as_u64().ok_or("discovery area missing")? as u8;
            let x = row["map_x"].as_u64().ok_or("discovery x missing")? as u8;
            let y = row["map_y"].as_u64().ok_or("discovery y missing")? as u8;
            cells.insert((region, x, y));
        }
    }
    let values = cells
        .into_iter()
        .map(|(region, x, y)| json!({"area":region,"map_x":x,"map_y":y,"value":1}))
        .collect::<Vec<_>>();
    Ok((
        json!(values),
        json!({"checkpoint":statistics(&checkpoint),"delta":statistics(&delta)}),
        checkpoint_ms,
    ))
}

pub fn map(store: &Store, run_id: &str, filter: &MapFilter) -> Result<Value> {
    let run_id = run(run_id)?;
    window(filter.from_ms, filter.to_ms, 3_600_000)?;
    let as_of_ms = filter.as_of_ms.unwrap_or(filter.to_ms);
    if as_of_ms < filter.to_ms || as_of_ms.saturating_sub(filter.to_ms) > 86_400_000 {
        return Err("as-of point must be at or after the window and within one day".into());
    }
    if filter.area.is_some_and(|area| !matches!(area, 0x10..=0x14)) {
        return Err("area must be a Metroid area byte from 16 through 20".into());
    }
    let area = filter
        .area
        .map_or(String::new(), |value| format!(" AND area={value}"));
    let time_field = if matches!(filter.metric, Metric::Work) {
        "selection_ms"
    } else {
        "event_ms"
    };
    let time = if matches!(filter.metric, Metric::Territory) {
        format!("event_ms < {}", filter.to_ms)
    } else if matches!(filter.metric, Metric::Work) {
        format!(
            "selection_ms >= {} AND selection_ms < {} AND event_ms < {as_of_ms}",
            filter.from_ms, filter.to_ms
        )
    } else {
        format!(
            "{time_field} >= {} AND {time_field} < {}",
            filter.from_ms, filter.to_ms
        )
    };
    let (cells, cost, checkpoint_ms) = if matches!(filter.metric, Metric::Territory) {
        territory(store, run_id, filter.to_ms, filter.area)?
    } else {
        let result = store.query(&format!(
            "SELECT area,map_x,map_y,{} AS value FROM observatory.events FINAL \
             WHERE run_id='{run_id}' AND {} AND {time}{area} \
             GROUP BY area,map_x,map_y ORDER BY area,map_y,map_x LIMIT 5120",
            filter.metric.sum(),
            filter.metric.kind()
        ))?;
        (data(&result), statistics(&result), None)
    };
    let state = status(store, run_id)?;
    Ok(json!({
        "schema_version":SCHEMA_VERSION,
        "run_id":run_id,
        "metric":filter.metric.name(),
        "unit":filter.metric.unit(),
        "window":{"from_ms":filter.from_ms,"to_ms":filter.to_ms,"boundary":"[from,to)","attribution":if matches!(filter.metric,Metric::Work) {"selection_time"} else {"observation_or_selection_time"}},
        "resolution_ms":1,
        "as_of_ms":as_of_ms,
        "provisional":filter.to_ms > state["finalized_through_ms"].as_u64().unwrap_or(0),
        "cells":cells,
        "checkpoint_ms":checkpoint_ms,
        "freshness_watermark_ms":state["freshness_watermark_ms"],
        "finalized_through_ms":state["finalized_through_ms"],
        "completeness":state["completeness"],
        "cost":cost
    }))
}

pub fn timeline(store: &Store, run_id: &str, filter: &TimelineFilter) -> Result<Value> {
    let run_id = run(run_id)?;
    window(filter.from_ms, filter.to_ms, 86_400_000)?;
    if filter.bucket_ms < 1000
        || filter.bucket_ms > 3_600_000
        || filter
            .to_ms
            .saturating_sub(filter.from_ms)
            .div_ceil(filter.bucket_ms)
            > 360
    {
        return Err(
            "timeline bucket must produce at most 360 points at one-second or coarser resolution"
                .into(),
        );
    }
    let result = store.query(&format!(
        "SELECT intDiv(event_ms,{bucket})*{bucket} AS bucket_ms, \
         countIf(kind='selection' OR kind='skip') AS selections, \
         sumIf(execution_work,kind='work') AS execution_work, \
         countIf(kind='discovery') AS new_places \
         FROM observatory.events FINAL WHERE run_id='{run_id}' \
         AND event_ms >= {from} AND event_ms < {to} \
         GROUP BY bucket_ms ORDER BY bucket_ms LIMIT 360",
        bucket = filter.bucket_ms,
        from = filter.from_ms,
        to = filter.to_ms
    ))?;
    let state = status(store, run_id)?;
    Ok(json!({
        "schema_version":SCHEMA_VERSION,
        "run_id":run_id,
        "window":filter,
        "points":data(&result),
        "resolution_ms":filter.bucket_ms,
        "freshness_watermark_ms":state["freshness_watermark_ms"],
        "completeness":state["completeness"],
        "cost":statistics(&result)
    }))
}

pub fn observations(store: &Store, run_id: &str, filter: &ObservationFilter) -> Result<Value> {
    let run_id = run(run_id)?;
    window(filter.from_ms, filter.to_ms, 3_600_000)?;
    let limit = filter.limit.unwrap_or(50);
    if limit == 0 || limit > 100 {
        return Err("observation limit must be 1 through 100".into());
    }
    if filter.map_x.is_some_and(|value| value >= 32)
        || filter.map_y.is_some_and(|value| value >= 32)
    {
        return Err("map coordinates must be 0 through 31".into());
    }
    if filter.area.is_some_and(|area| !matches!(area, 0x10..=0x14)) {
        return Err("area must be a Metroid area byte from 16 through 20".into());
    }
    let mut clauses = vec![
        format!("run_id='{run_id}'"),
        "kind='observation'".to_owned(),
        format!(
            "event_ms >= {} AND event_ms < {}",
            filter.from_ms, filter.to_ms
        ),
    ];
    if let Some(area) = filter.area {
        clauses.push(format!("area={area}"));
    }
    if let Some(x) = filter.map_x {
        clauses.push(format!("map_x={x}"));
    }
    if let Some(y) = filter.map_y {
        clauses.push(format!("map_y={y}"));
    }
    if let Some(health) = filter.min_health {
        clauses.push(format!("health>={health}"));
    }
    if let Some(missiles) = filter.min_missiles {
        clauses.push(format!("missiles>={missiles}"));
    }
    if let Some(bits) = filter.equipment_bits {
        clauses.push(format!("bitAnd(equipment,{bits})={bits}"));
    }
    if let Some(outcome) = &filter.outcome {
        if !matches!(outcome.as_str(), "runnable" | "terminal" | "failed") {
            return Err("unsupported outcome filter".into());
        }
        clauses.push(format!("outcome='{outcome}'"));
    }
    if let Some(after) = &filter.after {
        let Some((ms, id)) = after.split_once('-') else {
            return Err("invalid observation cursor".into());
        };
        let ms = ms.parse::<u64>()?;
        let id = id.parse::<u64>()?;
        clauses.push(format!("(event_ms,event_id)>({ms},{id})"));
    }
    let count_where = clauses
        .iter()
        .filter(|clause| !clause.starts_with("(event_ms,event_id)>"))
        .cloned()
        .collect::<Vec<_>>()
        .join(" AND ");
    let count_result = store.query(&format!(
        "SELECT count() AS matching_sampled_observations FROM observatory.events FINAL WHERE {count_where}"
    ))?;
    let count = count_result
        .get("data")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("matching_sampled_observations"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let where_clause = clauses.join(" AND ");
    let result = store.query(&format!(
        "SELECT event_ms,event_id,admission_sequence,reservation,parent_id,area,map_x,map_y,x,y,health,missiles,equipment,action_index,frame_count,outcome,sampled FROM observatory.events FINAL WHERE {where_clause} ORDER BY event_ms,event_id LIMIT {limit}"
    ))?;
    let rows = data(&result);
    let next = if rows
        .as_array()
        .is_some_and(|values| values.len() == usize::from(limit))
    {
        rows.as_array()
            .and_then(|values| values.last())
            .and_then(|last| {
                Some(format!(
                    "{}-{}",
                    last.get("event_ms")?.as_u64()?,
                    last.get("event_id")?.as_u64()?
                ))
            })
    } else {
        None
    };
    let state = status(store, run_id)?;
    Ok(json!({
        "schema_version":SCHEMA_VERSION,
        "run_id":run_id,
        "window":{"from_ms":filter.from_ms,"to_ms":filter.to_ms,"boundary":"[from,to)"},
        "sample_limit_per_admission":32,
        "interpretation":"Matches are sampled action observations from one state each. No match does not prove no such state occurred.",
        "matching_sampled_observations":count,
        "examples":rows,
        "next":next,
        "completeness":state["completeness"],
        "freshness_watermark_ms":state["freshness_watermark_ms"],
        "cost":statistics(&result)
    }))
}

pub fn draw_table(store: &Store, run_id: &str, at_ms: u64) -> Result<Value> {
    let run_id = run(run_id)?;
    let state = status(store, run_id)?;
    Ok(json!({
        "schema_version":SCHEMA_VERSION,
        "run_id":run_id,
        "at_ms":at_ms,
        "available":false,
        "policy":"alphabet_only",
        "reason":"The current Metroid input policy has no empirical draw table or published table version.",
        "completeness":state["completeness"],
        "freshness_watermark_ms":state["freshness_watermark_ms"]
    }))
}
