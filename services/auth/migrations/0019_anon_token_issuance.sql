-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0054 decision 2 - the issuer's per-account anonymous-token quota.
--
-- `POST /auth/tokens` counts every blinded element it evaluates against the
-- calling account's quota for (context, epoch). One row per account per
-- context per epoch; rows older than the previous epoch are pruned. The row
-- carries an account and a count - what the issuer already knows from the
-- session - and nothing about the tokens: no blinded element, no evaluation,
-- no nonce, so an issued token cannot be matched to the row that paid for it.
--
-- `context` is the frozen `lakko_anon_token::TokenContext` wire byte and
-- `epoch` is `floor(unix_secs / LAKKO_ANON_TOKEN_WINDOW_SECS)`.
--
-- This file is the single source of the schema: `services/auth` embeds a
-- byte-identical copy in its migrations directory and a test there asserts
-- the two match (`lakko_anon_token_server::ISSUANCE_MIGRATION_SQL`).
CREATE TABLE IF NOT EXISTS anon_token_issuance (
    account_id BINARY(16)       NOT NULL COMMENT 'UUIDv7 account id (raw bytes)',
    context    TINYINT UNSIGNED NOT NULL COMMENT 'TokenContext wire byte',
    epoch      BIGINT UNSIGNED  NOT NULL COMMENT 'issuance epoch',
    issued     INT UNSIGNED     NOT NULL COMMENT 'tokens evaluated for this account in this epoch',
    PRIMARY KEY (account_id, context, epoch),
    KEY idx_anon_token_issuance_epoch (epoch)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
