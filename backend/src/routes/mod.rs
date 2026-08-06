mod auth;
mod cases;
mod progress;
mod study;

use std::time::Duration;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;
use sqlx::PgPool;

use crate::state::AppState;

/// Upper bound on the health check's database probe. Sits above the pool's
/// own 5s `acquire_timeout` (see `db::connect`) so an unreachable database
/// surfaces sqlx's own error rather than a bare timeout, and well below the
/// global 15s request timeout so the probe can never hang a monitor's request.
const DB_PROBE_TIMEOUT: Duration = Duration::from_secs(8);

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .merge(auth::router())
        .merge(cases::router())
        .merge(progress::router())
        .merge(study::router())
}

/// `GET /health` — liveness and readiness in one.
///
/// A process that is running but can't reach Postgres serves nothing useful,
/// so the probe round-trips a query and answers 503 in that case. Uptime
/// monitors key off the status code; the body names which half failed.
///
/// - healthy: `200 {"status":"ok","database":"ok"}`
/// - database unreachable: `503 {"status":"degraded","database":"down"}`
async fn health(State(state): State<AppState>) -> Response {
    if db_healthy(&state.pool).await {
        (StatusCode::OK, Json(json!({ "status": "ok", "database": "ok" }))).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "degraded", "database": "down" })),
        )
            .into_response()
    }
}

/// Round-trip a trivial query to confirm the pool can still hand out a live
/// connection. False on any error, or if the probe outruns `DB_PROBE_TIMEOUT`.
///
/// The failure detail is logged, never returned — an unauthenticated endpoint
/// shouldn't narrate the database's problems to the internet.
pub async fn db_healthy(pool: &PgPool) -> bool {
    match tokio::time::timeout(DB_PROBE_TIMEOUT, sqlx::query("SELECT 1").execute(pool)).await {
        Ok(Ok(_)) => true,
        Ok(Err(err)) => {
            tracing::error!(error = ?err, "health check: database probe failed");
            false
        }
        Err(_) => {
            tracing::error!(
                timeout_secs = DB_PROBE_TIMEOUT.as_secs(),
                "health check: database probe timed out"
            );
            false
        }
    }
}
