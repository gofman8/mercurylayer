use std::time::Duration;

use sqlx::{Pool, Postgres, postgres::PgPoolOptions};

use crate::server_config::ServerConfig;

pub struct StateChainEntity {
    pub pool: Pool<Postgres>,
}

impl StateChainEntity {
    pub async fn new() -> Self {

        let config = ServerConfig::load();
        let connection_string = config.build_postgres_connection_string();

        // Pool sizing: 10 was far too low — under a few dozen concurrent clients every request
        // queued behind 10 connections and hit the 30s acquire timeout (PoolTimedOut), which the
        // request handlers then unwrapped into panics. Raise to a value that comfortably covers
        // concurrent signing + claims, tunable via DB_MAX_CONNECTIONS (keep it under the Postgres
        // server's own max_connections, shared with the lockbox's per-cosign connections).
        let max_conns: u32 = std::env::var("DB_MAX_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(50);
        let pool =
            PgPoolOptions::new()
            .max_connections(max_conns)
            .acquire_timeout(Duration::from_secs(30))  // Increase the timeout duration
            .connect_with(connection_string)
            .await
            .unwrap();

        StateChainEntity {
            pool,
        }
    }
}