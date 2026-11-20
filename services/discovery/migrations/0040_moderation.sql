-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0099 step 5: the moderator actions discovery applies, each one a
-- moderator's signed statement forwarded by the moderation service to
-- discovery's internal listener and checked here against the moderator list
-- under the pinned moderation root (src/moderation.rs).
--
-- 1. `identity_bundles.account_target`: the account target of the bundle's
--    signature key, `BLAKE3-derive-key("lakko/moderation/account-target/v1",
--    signature_pubkey)` (`lakko_moderation::target::account_target_hash`),
--    written by every writer of the key in the same statement. A statement
--    names an account by it. NULL on a row written before this migration (no
--    production data exists; nothing is backfilled).
--
-- 2. `handles.moderation`: 0 none, 1 delisted (out of search, publishing
--    refused), 2 blocked (also every handle write refused, and the row is
--    never freed, so the name stays held).
--
-- 3. `name_claims.kind` 3 (no schema change): a name a moderator took back
--    (TakeBackName). The claim makes every path that takes a name answer
--    `name-taken`; `moderation_name_holds` says until when.
--
-- 4. `moderation_name_holds`: one row per name held by a moderator.
--
-- 5. `moderation_hidden`: accounts a moderator removed from search; entering
--    Everyone is refused (`403 hidden-from-search`) while the row exists.
--
-- 6. `moderation_applied_statements`, `moderation_list_state`,
--    `moderation_target_decisions`: the statement ids applied (a replay is a
--    repeat), the highest moderator-list version seen (an older one is
--    refused), and the issue time of the newest statement applied to each
--    target (an older one changes nothing).

ALTER TABLE identity_bundles
    ADD COLUMN account_target BINARY(32) NULL
        COMMENT 'account_target_hash(signature_pubkey); NULL on a row written before 0040';

CREATE INDEX idx_identity_bundles_account_target ON identity_bundles (account_target);

ALTER TABLE handles
    ADD COLUMN moderation TINYINT UNSIGNED NOT NULL DEFAULT 0
        COMMENT '0 none, 1 delisted, 2 blocked (ADR-0099)';

CREATE TABLE IF NOT EXISTS moderation_name_holds (
    lookup_key    BINARY(64)       NOT NULL,
    until_ms      BIGINT UNSIGNED  NULL     COMMENT 'unix ms the hold ends; NULL = no end',
    statement_id  BINARY(16)       NOT NULL COMMENT 'the TakeBackName statement',
    log_index     BIGINT UNSIGNED  NOT NULL COMMENT 'its index in the moderation log',
    held_at_ms    BIGINT UNSIGNED  NOT NULL COMMENT 'unix ms discovery applied it',
    PRIMARY KEY (lookup_key),
    INDEX idx_moderation_name_holds_until (until_ms)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS moderation_hidden (
    account_id    BINARY(16)       NOT NULL,
    log_index     BIGINT UNSIGNED  NOT NULL COMMENT 'the HideFromSearch statement in the log',
    hidden_at_ms  BIGINT UNSIGNED  NOT NULL,
    PRIMARY KEY (account_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS moderation_applied_statements (
    statement_id  BINARY(16) NOT NULL,
    applied_at_ms BIGINT     NOT NULL COMMENT 'unix ms discovery applied it',
    PRIMARY KEY (statement_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS moderation_list_state (
    id           TINYINT UNSIGNED NOT NULL,
    list_version BIGINT UNSIGNED  NOT NULL COMMENT 'the highest moderator-list version seen',
    PRIMARY KEY (id),
    CONSTRAINT ck_moderation_list_state_one_row CHECK (id = 1)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS moderation_target_decisions (
    target_kind  TINYINT UNSIGNED NOT NULL COMMENT 'lakko_moderation TargetKind byte',
    target       VARBINARY(64)    NOT NULL,
    issued_at_ms BIGINT UNSIGNED  NOT NULL COMMENT 'issued_at_ms of the newest statement applied',
    PRIMARY KEY (target_kind, target)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
