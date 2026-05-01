-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 1.1a — auth service skeleton.
--
-- Schema будет дорастать в sprint-ах:
--   1.1b: OPAQUE-PAKE registration (envelope + masking key + server pubkey)
--   1.1c: WebAuthn credentials (passkey storage)
--   1.1d: PoW / Privacy Pass token state
--
-- Ничего PII здесь не хранится — только пseudonyms, OPAQUE-blinded
-- envelopes и public-key material (см. ADR-0005, security invariants
-- в CLAUDE.md).

-- Account = "минимальный анонимный аккаунт", идентифицируется случайным
-- UUIDv7-байтовым `id`. Username прибинживается в discovery service —
-- здесь только authentication state.
CREATE TABLE IF NOT EXISTS accounts (
    id            BINARY(16)  NOT NULL PRIMARY KEY COMMENT 'UUIDv7 (raw bytes)',
    created_at    TIMESTAMP   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    -- monotonic counter для KT log version-bump-ов при rotation (Phase 2)
    identity_version BIGINT UNSIGNED NOT NULL DEFAULT 1,
    -- account-level флаги (active, locked, deleted, etc.)
    flags         INT UNSIGNED NOT NULL DEFAULT 0
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Index для admin / debug запросов по дате создания.
CREATE INDEX idx_accounts_created_at ON accounts(created_at);
