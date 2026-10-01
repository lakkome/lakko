-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0054 decision 2 - a verifier's anonymous-token spent-set.
--
-- Every service that accepts anonymous tokens keeps this table in its own
-- database (the per-service-DB rule) and inserts a token's spend identifier
-- before honouring the request; the primary key makes the second spend of the
-- same token fail atomically. A token verifies only in its issuance epoch and
-- the one after, so rows whose epoch is older than the previous epoch are
-- pruned: such a token no longer verifies and cannot be replayed.
--
-- The spend identifier is a BLAKE3 derivation over (version, context, epoch,
-- nonce); it names no account and cannot be linked to an issuance.
--
-- This file is the single source of the schema: a consuming service copies it
-- byte for byte into its own migrations directory and asserts the copy against
-- `lakko_anon_token_server::SPENT_SET_MIGRATION_SQL` in a test.
CREATE TABLE IF NOT EXISTS anon_token_spent (
    context  TINYINT UNSIGNED NOT NULL COMMENT 'TokenContext wire byte',
    spend_id BINARY(32)       NOT NULL COMMENT 'BLAKE3 spend identifier',
    epoch    BIGINT UNSIGNED  NOT NULL COMMENT 'the token epoch, for pruning',
    PRIMARY KEY (context, spend_id),
    KEY idx_anon_token_spent_epoch (epoch)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
