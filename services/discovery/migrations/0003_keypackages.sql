-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 1.16e1 — public KeyPackage directory.
--
-- The discovery service already serves `username → account_id`
-- (from migration 0001) and `account_id → identity_bundle`
-- (from migration 0002). Sending an MLS Welcome to start a new
-- 1×1 group needs ONE more piece: the recipient's most-recent
-- MLS KeyPackage (RFC 9420 §10), which carries the recipient's
-- HPKE init_key for the DHKEM step. KeyPackages are PUBLIC by
-- design — they can be served from a directory just like the
-- identity bundle.
--
-- ## Phase-1 simplifications (documented gaps)
--
-- The architecture plan §3 calls for KeyPackages with a
-- 30-day TTL, last-resort fallback, and consume-on-use
-- semantics (one KeyPackage per receiver to preserve forward
-- secrecy on new groups). Phase 1.16e1 ships a far simpler
-- shape:
--
--   * **One row per account**, replaceable on every login. No
--     consume-on-use; the same KeyPackage may seed multiple
--     groups in this phase. Forward-secrecy regression vs. the
--     full design — acceptable for MVP, documented loud.
--   * **No TTL** — `updated_at` is informational. A future
--     pruner can age out rows that haven't been refreshed in
--     N days; not needed today.
--   * **Same trust gap as `/discovery/register` and
--     `/discovery/identity`**: a PoW token is required, but
--     the server trusts the client's claim of `account_id`
--     until Phase 1.5b lifts session tokens.
--
-- ## Sizes
--
-- A TLS-encoded MLS KeyPackage in our default ciphersuite
-- (`MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`) is ~400-600
-- bytes. `VARBINARY(2048)` gives 3-4× headroom; the FIPS
-- ciphersuite (P-384) and X.509 credentials in Phase 2+ would
-- push that closer to 1.5 KB.

CREATE TABLE IF NOT EXISTS keypackages (
    account_id        BINARY(16)      NOT NULL PRIMARY KEY
                        COMMENT 'UUIDv7 raw bytes — matches services/auth.accounts.id',
    keypackage_bytes  VARBINARY(2048) NOT NULL
                        COMMENT 'TLS-encoded MLS KeyPackage (RFC 9420 §10) ready to feed into Group::add_member',
    created_at        TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at        TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP
                        ON UPDATE CURRENT_TIMESTAMP
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
