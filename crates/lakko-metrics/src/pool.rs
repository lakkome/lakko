// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The database pool's connections, in use and idle.
//!
//! This crate knows no database; the service hands in a closure reading its
//! pool (`move || (pool.size(), pool.num_idle())` for an sqlx pool). How long
//! a request waits for a connection is not sampled: sqlx exposes no such
//! figure.

use std::time::Duration;

use crate::names;

/// How often the pool is sampled.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(15);

/// Spawn a task that sets `lakko_db_pool_connections{state}` from `sample`
/// every 15 seconds. `sample` returns the pool's open connections and how
/// many of them are idle.
pub fn spawn_pool_gauge<F>(sample: F) -> tokio::task::JoinHandle<()>
where
    F: Fn() -> (u32, usize) + Send + 'static,
{
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SAMPLE_INTERVAL);
        loop {
            tick.tick().await;
            let (open, idle) = sample();
            let idle = f64::from(u32::try_from(idle).unwrap_or(u32::MAX));
            let open = f64::from(open);
            metrics::gauge!(names::DB_POOL_CONNECTIONS, "state" => "idle").set(idle);
            metrics::gauge!(names::DB_POOL_CONNECTIONS, "state" => "in_use")
                .set((open - idle).max(0.0));
        }
    })
}
