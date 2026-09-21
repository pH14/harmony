// SPDX-License-Identifier: AGPL-3.0-or-later

use nes_observatory::{
    contract::Event,
    producer::random_id,
    query::{self, MapFilter, Metric, ObservationFilter},
    store::Store,
};
use serde_json::Value;

fn value(result: &Value, path: &[&str]) -> u64 {
    path.iter()
        .try_fold(result, |at, name| at.get(name))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn event(kind: &str, run: &str, session: &str, id: u64, at: u64) -> Event {
    let mut event = Event::new(kind);
    event.run_id = run.to_owned();
    event.session_id = session.to_owned();
    event.event_id = id;
    event.started_unix_ms = 1_789_950_000_000;
    event.event_ms = at;
    event
}

fn filter(from_ms: u64, to_ms: u64) -> ObservationFilter {
    ObservationFilter {
        from_ms,
        to_ms,
        area: Some(16),
        map_x: None,
        map_y: None,
        min_health: None,
        min_missiles: Some(5),
        equipment_bits: Some(4),
        outcome: None,
        after: None,
        limit: Some(50),
    }
}

#[test]
#[ignore = "requires pinned ClickHouse 26.3 and HARMONY_CLICKHOUSE_* credentials"]
fn clickhouse_history_filters_and_retry_are_consistent() {
    let store = Store::new(
        std::env::var("HARMONY_CLICKHOUSE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8123".to_owned()),
        std::env::var("HARMONY_CLICKHOUSE_USER").expect("test ClickHouse user"),
        std::env::var("HARMONY_CLICKHOUSE_PASSWORD").expect("test ClickHouse password"),
    )
    .expect("ClickHouse client");
    store.bootstrap().expect("schema");
    let run = random_id().expect("run ID");
    let session = random_id().expect("session ID");
    let mut events = vec![event("run_start", &run, &session, 1, 0)];
    let mut selection = event("selection", &run, &session, 2, 100);
    selection.selection_ms = 100;
    selection.area = 16;
    selection.map_x = 3;
    selection.map_y = 14;
    events.push(selection);
    let mut skip = event("skip", &run, &session, 3, 200);
    skip.area = 16;
    skip.map_x = 3;
    skip.map_y = 14;
    events.push(skip);
    let mut work = event("work", &run, &session, 4, 2500);
    work.selection_ms = 100;
    work.execution_work = 100;
    work.area = 16;
    work.map_x = 3;
    work.map_y = 14;
    events.push(work);
    let mut first = event("discovery", &run, &session, 5, 500);
    first.area = 16;
    first.map_x = 3;
    first.map_y = 14;
    events.push(first);
    let mut separate_missiles = event("observation", &run, &session, 6, 600);
    separate_missiles.area = 16;
    separate_missiles.map_x = 3;
    separate_missiles.map_y = 14;
    separate_missiles.health = 300;
    separate_missiles.missiles = 5;
    separate_missiles.equipment = 0;
    events.push(separate_missiles);
    let mut separate_gear = event("observation", &run, &session, 7, 700);
    separate_gear.area = 16;
    separate_gear.map_x = 3;
    separate_gear.map_y = 14;
    separate_gear.health = 300;
    separate_gear.missiles = 0;
    separate_gear.equipment = 4;
    events.push(separate_gear);
    let mut second = event("discovery", &run, &session, 8, 1500);
    second.area = 16;
    second.map_x = 4;
    second.map_y = 14;
    events.push(second);
    let mut joint = event("observation", &run, &session, 9, 1600);
    joint.area = 16;
    joint.map_x = 4;
    joint.map_y = 14;
    joint.health = 900;
    joint.missiles = 5;
    joint.equipment = 4;
    joint.admission_sequence = 2;
    events.push(joint);
    let mut checkpoint = event("territory_checkpoint", &run, &session, 10, 1000);
    let mut bits = [0_u8; 640];
    bits[(14 * 32 + 3) / 8] |= 1 << ((14 * 32 + 3) % 8);
    checkpoint.payload = bits.iter().map(|byte| format!("{byte:02x}")).collect();
    checkpoint.amount = 0;
    events.push(checkpoint);
    let mut end = event("run_end", &run, &session, 11, 3000);
    end.payload = "search_complete".to_owned();
    events.push(end);
    let mut batch = Vec::new();
    for event in &events {
        serde_json::to_writer(&mut batch, event).expect("event JSON");
        batch.push(b'\n');
    }
    let token = format!("b-{session}-0001");
    store.insert(&batch, &token).expect("first insert");
    store.insert(&batch, &token).expect("retry");
    let physical = store
        .query(&format!(
            "SELECT count() AS n FROM observatory.events WHERE run_id='{run}'"
        ))
        .expect("physical rows");
    assert_eq!(value(&physical["data"][0], &["n"]), 11);
    let status = query::status(&store, &run).expect("status");
    assert_eq!(value(&status, &["telemetry", "selections"]), 2);
    assert_eq!(value(&status, &["telemetry", "execution_work"]), 100);
    assert_eq!(status["completeness"]["complete"], true);

    let selections = query::map(
        &store,
        &run,
        &MapFilter {
            metric: Metric::Selections,
            from_ms: 0,
            to_ms: 1000,
            area: Some(16),
            as_of_ms: None,
        },
    )
    .expect("selections");
    assert_eq!(value(&selections["cells"][0], &["value"]), 2);
    let work = query::map(
        &store,
        &run,
        &MapFilter {
            metric: Metric::Work,
            from_ms: 0,
            to_ms: 1000,
            area: Some(16),
            as_of_ms: None,
        },
    )
    .expect("work");
    assert!(work["cells"].as_array().expect("cells").is_empty());
    let eventual_work = query::map(
        &store,
        &run,
        &MapFilter {
            metric: Metric::Work,
            from_ms: 0,
            to_ms: 1000,
            area: Some(16),
            as_of_ms: Some(3001),
        },
    )
    .expect("eventual work");
    assert_eq!(value(&eventual_work["cells"][0], &["value"]), 100);
    let earlier = query::map(
        &store,
        &run,
        &MapFilter {
            metric: Metric::Territory,
            from_ms: 0,
            to_ms: 1000,
            area: Some(16),
            as_of_ms: None,
        },
    )
    .expect("earlier territory");
    assert_eq!(earlier["cells"].as_array().expect("cells").len(), 1);
    let later = query::map(
        &store,
        &run,
        &MapFilter {
            metric: Metric::Territory,
            from_ms: 1000,
            to_ms: 2000,
            area: Some(16),
            as_of_ms: None,
        },
    )
    .expect("later territory");
    assert_eq!(later["cells"].as_array().expect("cells").len(), 2);
    assert_eq!(value(&later, &["checkpoint_ms"]), 1000);
    let no_joint = query::observations(&store, &run, &filter(0, 1000)).expect("separate states");
    assert_eq!(value(&no_joint, &["matching_sampled_observations"]), 0);
    let joint = query::observations(&store, &run, &filter(0, 2000)).expect("joint state");
    assert_eq!(value(&joint, &["matching_sampled_observations"]), 1);
    assert_eq!(value(&joint["examples"][0], &["admission_sequence"]), 2);
    let gap_run = random_id().expect("gap run ID");
    let mut gap_events = vec![
        event("run_start", &gap_run, &session, 1, 0),
        event("run_end", &gap_run, &session, 3, 100),
    ];
    gap_events[1].payload = "search_complete".to_owned();
    let mut gap_batch = Vec::new();
    for event in gap_events {
        serde_json::to_writer(&mut gap_batch, &event).expect("gap JSON");
        gap_batch.push(b'\n');
    }
    store
        .insert(&gap_batch, &format!("b-{session}-gap"))
        .expect("gap insert");
    let gap_status = query::status(&store, &gap_run).expect("gap status");
    assert_eq!(gap_status["completeness"]["complete"], false);
    assert_eq!(value(&gap_status, &["completeness", "sequence_gaps"]), 1);

    let missing_start = random_id().expect("missing-start run ID");
    let mut end_only = event("run_end", &missing_start, &session, 1, 100);
    end_only.payload = "search_complete".to_owned();
    let mut end_batch = serde_json::to_vec(&end_only).expect("end JSON");
    end_batch.push(b'\n');
    store
        .insert(&end_batch, &format!("b-{session}-end"))
        .expect("end insert");
    let end_status = query::status(&store, &missing_start).expect("missing-start status");
    assert_eq!(end_status["completeness"]["complete"], false);
    assert!(query::status(&store, &random_id().expect("absent ID")).is_err());
}
