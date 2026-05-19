-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 3b4 forward (call-JWT issuance) — companion to 3b1a's
-- SFU scaffolding. Stores the Ed25519 seed the auth service
-- uses to sign call JWTs. Single-row table — `id = 1` is the
-- only meaningful row, mirroring `opaque_server_setup`.
--
-- The PUBLIC half is published at GET /.well-known/jwks.json;
-- the SFU process fetches it at startup + refreshes every 24 h
-- per ADR-0008 §"Auth token format".
--
-- Rotation is out of scope for 3b4 — when we get there, the
-- table can grow a `kid` column + the handler can return all
-- active keys in the JWKS doc, with the most-recent one used
-- for new mints. For now, one keypair, persisted, generated
-- on first run via `load_or_init_call_signing_key`.

CREATE TABLE IF NOT EXISTS call_jwt_signing_key (
    id    TINYINT UNSIGNED NOT NULL PRIMARY KEY CHECK (id = 1),
    -- 32-byte Ed25519 seed. The signing keypair is rebuilt
    -- from this seed at every process start; the public key
    -- is then derived deterministically.
    seed  BINARY(32)       NOT NULL,
    -- Bookkeeping — when the row was first inserted.
    created_at_ms BIGINT   NOT NULL DEFAULT (UNIX_TIMESTAMP() * 1000)
) ENGINE = InnoDB
  DEFAULT CHARSET = utf8mb4
  COLLATE = utf8mb4_unicode_ci;
