// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::BTreeMap,
    error::Error,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use reqwest::blocking::Client;
use serde_json::Value;

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const CREATE_EVENTS: &str = "
CREATE TABLE IF NOT EXISTS observatory.events (
    schema_version UInt16,
    run_id String,
    session_id String,
    event_id UInt64,
    started_unix_ms UInt64,
    event_ms UInt64,
    selection_ms UInt64,
    kind LowCardinality(String),
    selection_id UInt64,
    reservation UInt64,
    admission_sequence UInt64,
    parent_id UInt64,
    area UInt8,
    map_x UInt8,
    map_y UInt8,
    x UInt8,
    y UInt8,
    health UInt16,
    missiles UInt8,
    equipment UInt8,
    execution_work UInt64,
    amount UInt32,
    action_index UInt32,
    frame_count UInt64,
    outcome LowCardinality(String),
    sampled UInt8,
    payload String
)
ENGINE = ReplacingMergeTree
PARTITION BY toYYYYMM(toDateTime(started_unix_ms / 1000))
ORDER BY (run_id, event_ms, session_id, event_id)
TTL toDateTime(started_unix_ms / 1000) + INTERVAL 7 DAY
SETTINGS non_replicated_deduplication_window = 100000
";

#[derive(Clone)]
pub struct Store {
    client: Client,
    url: String,
    user: String,
    password: String,
    status_cache: Arc<Mutex<BTreeMap<String, (Instant, Value)>>>,
}

impl Store {
    pub fn new(url: String, user: String, password: String) -> Result<Self> {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("ClickHouse URL must use HTTP or HTTPS".into());
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(2))
            .build()?;
        Ok(Self {
            client,
            url,
            user,
            password,
            status_cache: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    fn request(&self, sql: &str, body: Vec<u8>, token: Option<&str>) -> Result<String> {
        let mut request = self
            .client
            .post(&self.url)
            .basic_auth(&self.user, Some(&self.password))
            .query(&[("query", sql)])
            .body(body);
        if let Some(token) = token {
            request = request.query(&[
                ("insert_deduplication_token", token),
                ("insert_deduplicate", "1"),
                ("async_insert", "1"),
                ("wait_for_async_insert", "1"),
            ]);
        }
        let response = request.send()?;
        let status = response.status();
        let text = response.text()?;
        if !status.is_success() {
            return Err(format!(
                "ClickHouse {status}: {}",
                text.chars().take(512).collect::<String>()
            )
            .into());
        }
        Ok(text)
    }

    pub fn bootstrap(&self) -> Result<()> {
        self.request(
            "CREATE DATABASE IF NOT EXISTS observatory",
            Vec::new(),
            None,
        )?;
        self.request(CREATE_EVENTS, Vec::new(), None)?;
        Ok(())
    }

    pub fn insert(&self, batch: &[u8], token: &str) -> Result<()> {
        if batch.len() > 1_048_576 {
            return Err("telemetry batch exceeds one MiB".into());
        }
        if token.is_empty()
            || token.len() > 128
            || !token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err("telemetry batch token is malformed".into());
        }
        self.request(
            "INSERT INTO observatory.events FORMAT JSONEachRow",
            batch.to_vec(),
            Some(token),
        )?;
        Ok(())
    }

    pub fn query(&self, sql: &str) -> Result<Value> {
        if sql.len() > 4096 {
            return Err("query exceeds SQL length cap".into());
        }
        let bounded = format!(
            "{sql} SETTINGS max_execution_time=2, max_memory_usage=268435456, max_result_rows=10000, max_result_bytes=8388608, max_bytes_to_read=1073741824, max_threads=2 FORMAT JSON"
        );
        let text = self.request(&bounded, Vec::new(), None)?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn cached_status(&self, run_id: &str) -> Option<Value> {
        let cache = self.status_cache.lock().ok()?;
        let (stored, value) = cache.get(run_id)?;
        let ttl = if value["completeness"]["complete"].as_bool() == Some(true) {
            Duration::from_secs(30)
        } else {
            Duration::from_millis(1500)
        };
        (stored.elapsed() < ttl).then(|| value.clone())
    }

    #[allow(clippy::disallowed_methods)]
    pub fn cache_status(&self, run_id: &str, value: &Value) {
        if let Ok(mut cache) = self.status_cache.lock() {
            if cache.len() >= 100 {
                cache.clear();
            }
            cache.insert(run_id.to_owned(), (Instant::now(), value.clone()));
        }
    }

    pub fn ready(&self) -> Result<String> {
        self.request("SELECT version()", Vec::new(), None)
    }
}
