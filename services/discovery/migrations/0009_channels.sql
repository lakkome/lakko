-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 4 — G4-5: channel @name discovery via OPRF.
--
-- Mirrors the username registry from 0001_init.sql but keys
-- the row by `channel_id` (16-byte pseudonym, ADR-0010) rather
-- than `account_id`. The server never sees the plaintext
-- channel name — clients submit a blinded element through
-- `/discovery/oprf` (which is namespace-agnostic), apply the
-- static `K`, finalise locally to an OPRF output, then call
-- `/discovery/channel/register` or `/discovery/channel/lookup`
-- with the hash.
--
-- Same privacy properties as the username table — database leak
-- without K is useless; network observers see only blinded
-- points; running server with K can enumerate small
-- dictionaries offline (mitigated by PoW on lookups + register).
--
-- Why a separate table rather than a `kind` column on
-- `usernames`: two distinct namespaces with distinct id types
-- (UUIDv7 account_id vs 16-byte channel_id) — keeping them
-- physically separate makes the schema readable and lets
-- queries hit a tighter index.

CREATE TABLE IF NOT EXISTS channel_names (
    -- VOPRF Ristretto255-SHA512 output (one SHA-512 digest =
    -- 64 bytes). Same shape the username table uses.
    oprf_hash   VARBINARY(64) NOT NULL PRIMARY KEY,
    -- 16-byte channel pseudonym (ADR-0010 §"Wire surfaces").
    -- Rendered as 32-char lowercase hex on the URL surface in
    -- services/channels; the bytes are the parsed binary form.
    channel_id  VARBINARY(16) NOT NULL,
    created_at  TIMESTAMP     NOT NULL DEFAULT CURRENT_TIMESTAMP
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE INDEX idx_channel_names_channel_id
    ON channel_names (channel_id);
