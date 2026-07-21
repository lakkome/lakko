-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0037 — auth-signed backup-head rollback commitment (G-11).
--
-- `POST /auth/backup/head` mints a `BackupHead` whose `backup_version` is a
-- per-account, auth-assigned, strictly-monotone counter. Because the version is
-- server-assigned and never client-supplied, a regression is structurally
-- impossible — auth only ever signs its counter's next value, so an attacker
-- cannot induce a lower or duplicate version, and a returning client can trust
-- the version as a monotone latest-marker (ADR-0037 § "the load-bearing design
-- decision"). This table holds that counter and nothing else.
--
-- Only the monotone counter is stored — `account_id` and an integer `version`.
-- No key material, no envelope bytes, no envelope hash (auth signs the
-- client-presented hash without persisting it), no timestamp beyond the
-- bookkeeping `updated_at` (security invariant #1 / #2: the server sees only
-- ciphertext + public artefacts). This is the deviation from the ADR-0036
-- stateless genesis mint and the sole reason a migration is required here.

-- One row per account, holding the highest `backup_version` ever minted for it.
-- `account_id` is stored as the raw 16 UUIDv7 bytes (the `AuthenticatedAccount`
-- extractor supplies them). The read-bump is done in a single transaction using
-- the MariaDB `LAST_INSERT_ID(expr)` idiom so no version is ever signed twice
-- and versions strictly increase per account (see `src/backup_head.rs`).
CREATE TABLE IF NOT EXISTS backup_head_versions (
    account_id BINARY(16)     NOT NULL PRIMARY KEY COMMENT 'UUIDv7 account id (raw bytes)',
    version    BIGINT UNSIGNED NOT NULL COMMENT 'highest auth-assigned monotone backup version',
    updated_at TIMESTAMP        NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP
        COMMENT 'last mint for this account'
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
