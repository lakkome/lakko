// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! DB-gated end-to-end test for the load-from-DB rebuild path.
//!
//! Gated on `LAKKO_TRANSPARENCY_TEST_DATABASE_URL` — skips (does not
//! fail) when no test DB is configured, mirroring the auth service's
//! `LAKKO_AUTH_TEST_DATABASE_URL`-gated pattern.
//!
//! Run locally against a throwaway DB:
//!
//! ```sh
//! LAKKO_TRANSPARENCY_TEST_DATABASE_URL=mysql://lakko:devpass@127.0.0.1:3306/lakko_transparency_test \
//!   cargo test -p lakko-svc-transparency --test db_reload_e2e -- --nocapture
//! ```
//!
//! The binary crate's modules are not visible from an integration test,
//! so this drives the public `lakko-transparency` crate directly to
//! prove the persistence contract: append N entries, persist their
//! rows, then rebuild a FRESH `InMemoryLog` from those rows in `idx`
//! order and assert the rebuilt root is byte-identical. This is exactly
//! what `store::LogStore::load` does at boot.

use lakko_crypto::signing;
use lakko_transparency::{verify_inclusion, InMemoryLog, LogEntry};
use sqlx::mysql::MySqlPoolOptions;
use sqlx::{MySqlPool, Row as _};

const MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

fn entry(name: &str, version: u64, pk: u8) -> LogEntry {
    LogEntry {
        username_hash: lakko_crypto::hash::blake3(name.as_bytes()),
        identity_pubkey: vec![pk; 32],
        version,
    }
}

/// Append a leaf row mirroring `LogStore::append`'s INSERT.
async fn insert_row(pool: &MySqlPool, idx: u64, e: &LogEntry, is_reset: bool) {
    sqlx::query(
        "INSERT INTO log_entries (idx, username_hash, identity_pubkey, version, is_reset) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(idx)
    .bind(&e.username_hash[..])
    .bind(&e.identity_pubkey[..])
    .bind(e.version)
    .bind(is_reset)
    .execute(pool)
    .await
    .expect("insert log row");
}

#[tokio::test]
async fn reload_from_db_rebuilds_identical_root() {
    let Ok(url) = std::env::var("LAKKO_TRANSPARENCY_TEST_DATABASE_URL") else {
        eprintln!(
            "skipping reload_from_db_rebuilds_identical_root: \
             LAKKO_TRANSPARENCY_TEST_DATABASE_URL unset"
        );
        return;
    };

    let pool = MySqlPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("connect test DB");
    MIGRATIONS.run(&pool).await.expect("run migrations");

    // Clean slate so repeated runs don't collide on the PK.
    sqlx::query("DELETE FROM log_entries")
        .execute(&pool)
        .await
        .expect("clear log_entries");

    // A fixed operator seed so both the "live" and "reloaded" logs sign
    // under the same key — the persistence path rebuilds the operator
    // key from the same DB seed in production.
    let seed = [0x77u8; 32];
    let operator = signing::SigningKey::from_seed(&seed);

    // --- Phase 1: build a live log + persist every leaf. ---
    let mut live = InMemoryLog::new(operator);
    let entries = [
        (entry("alice", 1, 0x01), false),
        (entry("bob", 1, 0x02), false),
        (entry("alice", 2, 0x03), false), // rotation
        (entry("carol", 1, 0x04), true),  // fresh-start reset
    ];
    for (e, is_reset) in &entries {
        let idx = live.append(e.clone());
        insert_row(&pool, idx, e, *is_reset).await;
    }
    let live_root = live.current_root();
    let live_signed = live.sign_current_root(1_700_000_000);

    // --- Phase 2: rebuild a FRESH log from the DB rows (what
    // `LogStore::load` does). ---
    let rows = sqlx::query(
        "SELECT idx, username_hash, identity_pubkey, version, is_reset \
         FROM log_entries ORDER BY idx ASC",
    )
    .fetch_all(&pool)
    .await
    .expect("select rows");

    let mut reloaded = InMemoryLog::new(signing::SigningKey::from_seed(&seed));
    let mut reset_flags = Vec::new();
    for row in &rows {
        let db_idx: u64 = row.get("idx");
        let uh: Vec<u8> = row.get("username_hash");
        let pk: Vec<u8> = row.get("identity_pubkey");
        let version: u64 = row.get("version");
        let is_reset: i8 = row.get("is_reset");
        let mut username_hash = [0u8; 32];
        username_hash.copy_from_slice(&uh);
        let assigned = reloaded.append(LogEntry {
            username_hash,
            identity_pubkey: pk,
            version,
        });
        assert_eq!(assigned, db_idx, "rebuilt index must match DB idx");
        reset_flags.push(is_reset != 0);
    }

    // The rebuilt tree must be byte-identical to the live one.
    assert_eq!(reloaded.current_root(), live_root, "rebuilt root diverged");
    assert_eq!(reloaded.size(), entries.len() as u64);

    // The fresh-start marker round-tripped: only carol's entry (idx 3).
    assert_eq!(reset_flags, vec![false, false, false, true]);

    // A proof built against the reloaded tree verifies under the live
    // signed root (same root, same operator key).
    for i in 0..entries.len() as u64 {
        let proof = reloaded.prove_inclusion(i).expect("proof");
        let e = reloaded.entry(i).expect("entry");
        verify_inclusion(e, &proof, &live_signed).expect("inclusion verifies post-reload");
    }

    // Cleanup so the table is empty for the next run.
    sqlx::query("DELETE FROM log_entries")
        .execute(&pool)
        .await
        .expect("clear log_entries");
    pool.close().await;
}
