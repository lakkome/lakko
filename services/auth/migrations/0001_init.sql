-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 1.1a — auth service skeleton.
--
-- The schema grows over the next sprints:
--   1.1b: OPAQUE-PAKE registration (envelope + masking key + server pubkey)
--   1.1c: OPAQUE-PAKE login state (server-stateful between start/finish)
--   1.1d: BIP39 mnemonic recovery state
--   1.1e: PoW / Privacy Pass token state
--
-- Nothing PII is stored here — only pseudonyms, OPAQUE-blinded envelopes,
-- and public-key material (see ADR-0005 and the security invariants in
-- CLAUDE.md).

-- An account is a "minimal anonymous account" identified by a random
-- UUIDv7 byte `id`. Usernames are bound in the discovery service —
-- this table holds authentication state only.
CREATE TABLE IF NOT EXISTS accounts (
    id            BINARY(16)  NOT NULL PRIMARY KEY COMMENT 'UUIDv7 (raw bytes)',
    created_at    TIMESTAMP   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    -- Monotonic counter for KT-log version bumps on identity rotation
    -- (Phase 2).
    identity_version BIGINT UNSIGNED NOT NULL DEFAULT 1,
    -- Account-level flags (active, locked, deleted, etc.).
    flags         INT UNSIGNED NOT NULL DEFAULT 0
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Index for admin / debug queries by creation time.
CREATE INDEX idx_accounts_created_at ON accounts(created_at);
