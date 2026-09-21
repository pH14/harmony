// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    Json, Router,
    extract::{Path as AxumPath, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::{
    query::{self, MapFilter, ObservationFilter, TimelineFilter},
    store::Store,
};

type ApiResult = std::result::Result<Json<Value>, (StatusCode, Json<Value>)>;

#[derive(Clone)]
struct AppState {
    store: Store,
    slots: Arc<Semaphore>,
    static_dir: PathBuf,
}

async fn bounded(
    state: AppState,
    call: impl FnOnce(Store) -> crate::store::Result<Value> + Send + 'static,
) -> ApiResult {
    let permit = state.slots.clone().try_acquire_owned().map_err(|_| {
        (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":"query concurrency limit reached"})),
        )
    })?;
    let outcome = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        call(state.store)
    })
    .await
    .map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
    })?;
    outcome.map(Json).map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":error.to_string()})),
        )
    })
}

async fn runs(State(state): State<AppState>) -> ApiResult {
    bounded(state, |store| query::runs(&store)).await
}

async fn status(AxumPath(run_id): AxumPath<String>, State(state): State<AppState>) -> ApiResult {
    bounded(state, move |store| query::status(&store, &run_id)).await
}

async fn map(
    AxumPath(run_id): AxumPath<String>,
    State(state): State<AppState>,
    Query(filter): Query<MapFilter>,
) -> ApiResult {
    bounded(state, move |store| query::map(&store, &run_id, &filter)).await
}

async fn timeline(
    AxumPath(run_id): AxumPath<String>,
    State(state): State<AppState>,
    Query(filter): Query<TimelineFilter>,
) -> ApiResult {
    bounded(state, move |store| {
        query::timeline(&store, &run_id, &filter)
    })
    .await
}

async fn observations(
    AxumPath(run_id): AxumPath<String>,
    State(state): State<AppState>,
    Query(filter): Query<ObservationFilter>,
) -> ApiResult {
    bounded(state, move |store| {
        query::observations(&store, &run_id, &filter)
    })
    .await
}

#[derive(Deserialize)]
struct DrawAt {
    at_ms: u64,
}

async fn draw_table(
    AxumPath(run_id): AxumPath<String>,
    State(state): State<AppState>,
    Query(at): Query<DrawAt>,
) -> ApiResult {
    bounded(state, move |store| {
        query::draw_table(&store, &run_id, at.at_ms)
    })
    .await
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let result = tokio::task::spawn_blocking(move || state.store.ready()).await;
    match result {
        Ok(Ok(version)) => (
            StatusCode::OK,
            Json(json!({"ready":true,"clickhouse":version.trim()})),
        ),
        Ok(Err(error)) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ready":false,"error":error.to_string()})),
        ),
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ready":false,"error":error.to_string()})),
        ),
    }
}

async fn index(State(state): State<AppState>) -> Response {
    static_file(&state.static_dir, "index.html").await
}

async fn asset(AxumPath(path): AxumPath<String>, State(state): State<AppState>) -> Response {
    if path.contains("..") || path.contains('\\') || path.starts_with('/') {
        return StatusCode::BAD_REQUEST.into_response();
    }
    static_file(&state.static_dir, &format!("assets/{path}")).await
}

async fn static_file(root: &Path, path: &str) -> Response {
    match tokio::fs::read(root.join(path)).await {
        Ok(bytes) => {
            let mime = match Path::new(path).extension().and_then(|value| value.to_str()) {
                Some("js") => "text/javascript; charset=utf-8",
                Some("css") => "text/css; charset=utf-8",
                Some("svg") => "image/svg+xml",
                Some("png") => "image/png",
                _ => "text/html; charset=utf-8",
            };
            ([(header::CONTENT_TYPE, mime)], bytes).into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn serve(store: Store, address: &str, static_dir: PathBuf) -> crate::store::Result<()> {
    let state = AppState {
        store,
        slots: Arc::new(Semaphore::new(4)),
        static_dir,
    };
    let app = Router::new()
        .route("/api/v1/runs", get(runs))
        .route("/api/v1/runs/{run_id}/status", get(status))
        .route("/api/v1/runs/{run_id}/map", get(map))
        .route("/api/v1/runs/{run_id}/timeline", get(timeline))
        .route("/api/v1/runs/{run_id}/observations", get(observations))
        .route("/api/v1/runs/{run_id}/draw-table", get(draw_table))
        .route("/api/v1/health", get(health))
        .route("/", get(index))
        .route("/assets/{*path}", get(asset))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
