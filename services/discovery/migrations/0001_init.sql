-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 1.5 — Discovery service: username table via OPRF (RFC 9497).
--
-- The server NEVER stores plaintext `@username`. Clients submit a
-- blinded element through `/discovery/oprf`; the server applies its
-- static secret `K` and returns the evaluated element; the client
-- finalises locally and gets the OPRF output, which is the actual
-- DB key for `/discovery/register` and `/discovery/lookup`.
--
-- Properties:
--   * Database leak (without K): table contents are useless — OPRF
--     output cannot be reversed to plaintext usernames.
--   * Network observers: never see plaintext usernames; only
--     uniformly-distributed blinded curve points.
--   * Running server with K: can in principle enumerate any small
--     dictionary of usernames offline. Not perfect privacy, but the
--     logs and DB do not contain plaintext.
--
-- Ciphersuite: Ristretto255-SHA512 (RFC 9497 §3.2.1) — matches the
-- OPAQUE-PAKE choice in `services/auth`.

-- ----------------------------------------------------------------------------
-- Server-static OPRF setup. Single row enforced by CHECK(id=1).
-- Generated on first run, read-only afterwards.
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS oprf_server_setup (
    id          TINYINT       NOT NULL PRIMARY KEY DEFAULT 1,
    -- Serialised `OprfServer<Ristretto255>` state. The voprf crate
    -- stores a single Ristretto255 scalar (32 bytes); 128 bytes leaves
    -- room for any future serialisation framing.
    state_bytes VARBINARY(128) NOT NULL,
    created_at  TIMESTAMP     NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT chk_oprf_server_setup_singleton CHECK (id = 1)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- ----------------------------------------------------------------------------
-- Username → account_id mapping, keyed by OPRF output.
--
-- OPRF output for Ristretto255-SHA512 is 64 bytes (one SHA-512 digest).
-- The client computes it locally after a /discovery/oprf round trip
-- and presents it on /discovery/register and /discovery/lookup.
--
-- KNOWN PHASE-1 GAP: register currently trusts the client's claimed
-- `account_id` — there is no proof of ownership. Phase 1.5b will gate
-- /discovery/register behind an auth-issued session token. For now,
-- usernames are immutable (no UPDATE/DELETE) and PoW-gated; first-come
-- first-served. Squatting is recoverable by table reset before Phase 2.
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS usernames (
    oprf_hash   BINARY(64)  NOT NULL PRIMARY KEY,
    account_id  BINARY(16)  NOT NULL COMMENT 'UUIDv7 raw bytes — same as services/auth',
    created_at  TIMESTAMP   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    -- Reverse lookup ("which usernames does this account own?") for
    -- ops queries; not exposed via the public API in Phase 1.
    INDEX idx_usernames_account (account_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- ----------------------------------------------------------------------------
-- PoW challenges — same shape as services/auth. The two services have
-- separate databases (per the per-service DB isolation rule), so the
-- table is duplicated rather than shared. Phase 2 should DRY this into
-- a `lakko-pow` workspace crate.
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS pow_challenges (
    seed         BINARY(16)   NOT NULL PRIMARY KEY,
    effort_bits  TINYINT UNSIGNED NOT NULL,
    expires_at   TIMESTAMP(3) NOT NULL,
    INDEX idx_pow_challenges_expires_at (expires_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
