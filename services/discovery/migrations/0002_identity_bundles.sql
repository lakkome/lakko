-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 1.16d3a — public identity-bundle directory.
--
-- The discovery service already maps `username → account_id` via the
-- `usernames` table. To send a `lakko-contacts::ContactRequest` the
-- sender ALSO needs the recipient's Ed25519 identity public key
-- (signature pinning) and the recipient's X25519 public key (HPKE
-- sealed envelope target). Both are PUBLIC — putting them in a
-- queryable directory leaks no secret.
--
-- One row per account_id. The bundle is uploaded once at signup and
-- replaced atomically via INSERT ... ON DUPLICATE KEY UPDATE when the
-- user rotates keys (Phase 1.16d3c+ — for now, signup is the only
-- writer).
--
-- KNOWN PHASE-1 GAP: `POST /discovery/identity` currently trusts the
-- client's claim of `account_id`, same as `POST /discovery/register`.
-- A PoW token is required, but no proof of ownership beyond that.
-- Phase 1.5b will gate this endpoint on an auth-issued session token
-- (JWT signed by `services/auth`). Until then, anyone with PoW can
-- overwrite an account's identity bundle — squatting damage is
-- recoverable by manual table reset before beta launch.
--
-- Sizes:
--   * Ed25519 public key: 32 bytes (SignatureKey, RFC 8032).
--   * X25519 public key:  32 bytes (HPKE-Base init_key).
-- Hard-coded as `BINARY(32)` columns; if a future cipher suite needs
-- larger keys, add a new column rather than widening this one — the
-- on-the-wire size is part of the contract with the client.

CREATE TABLE IF NOT EXISTS identity_bundles (
    account_id        BINARY(16) NOT NULL PRIMARY KEY
                        COMMENT 'UUIDv7 raw bytes — matches services/auth.accounts.id',
    signature_pubkey  BINARY(32) NOT NULL
                        COMMENT 'Ed25519 identity verification key',
    x25519_pubkey     BINARY(32) NOT NULL
                        COMMENT 'X25519 HPKE init_key for sealed-envelope ContactRequests',
    created_at        TIMESTAMP  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at        TIMESTAMP  NOT NULL DEFAULT CURRENT_TIMESTAMP
                        ON UPDATE CURRENT_TIMESTAMP
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
