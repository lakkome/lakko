-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0037 F5b — generous, fail-open per-account rate-limit on the backup-head
-- mint (`POST /auth/backup/head`).
--
-- The mint is bearer-authed and own-account only: a holder of a valid session
-- token can call it repeatedly for THEIR OWN account. That is harmless to other
-- accounts (the monotone `backup_head_versions` counter is per-account and never
-- client-supplied), but it is unbounded DB write pressure. This migration adds
-- the single bookkeeping table for a cheap defence-in-depth cap — "at most N
-- mints per account per fixed window W" (see `src/backup_head.rs`). The cap is
-- deliberately generous (well above any legitimate auto-backup cadence) and
-- fails OPEN on a store error, mirroring the ADR-0033 throttle: a rate-limit is
-- an availability gate, not a security invariant, so failing closed would
-- self-DoS legitimate backups.
--
-- Only bookkeeping is stored — `account_id`, the current window start, and a
-- counter. No key material, no envelope bytes, no envelope hash (security
-- invariant #1 / #2: the server sees only ciphertext + public artefacts).

-- One row per account (same cardinality as `backup_head_versions` — bounded by
-- construction, no unbounded event rows, so no pruner is required). A fixed
-- window: each mint upserts this row, resetting the counter to 1 when the prior
-- window has elapsed or incrementing it otherwise, all in one atomic statement
-- (the `ON DUPLICATE KEY UPDATE` reads the pre-update `window_start` for both
-- branch conditions). `account_id` is the raw 16 UUIDv7 bytes.
CREATE TABLE IF NOT EXISTS backup_head_mint_limits (
    account_id   BINARY(16)      NOT NULL PRIMARY KEY COMMENT 'UUIDv7 account id (raw bytes)',
    window_start TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3)
        COMMENT 'start of the current fixed rate-limit window',
    mint_count   BIGINT UNSIGNED NOT NULL COMMENT 'mints observed in the current window'
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
