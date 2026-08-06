//! `GET /health` probes the database rather than reporting a hardcoded "ok",
//! so an uptime monitor sees a real outage when Postgres is unreachable.

mod common;

use common::TestDb;
use cube_backend::routes::db_healthy;

#[tokio::test]
async fn reports_healthy_against_a_live_pool() {
    let db = TestDb::new().await;
    assert!(db_healthy(&db.pool).await);
}

#[tokio::test]
async fn reports_unhealthy_once_the_pool_is_closed() {
    let db = TestDb::new().await;
    assert!(db_healthy(&db.pool).await, "sanity: pool starts healthy");

    // A closed pool is the cheapest stand-in for "the database went away
    // while the process kept running" — the exact case the old hardcoded
    // handler reported as healthy.
    db.pool.close().await;

    assert!(!db_healthy(&db.pool).await);
}
