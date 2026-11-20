-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0098 section 4: account suspension.
--
-- 1. `account_suspensions`: one row per suspended account. Suspended means a
--    row with `until_ms IS NULL` (no end) or `until_ms > now`; an expired row
--    is ignored and the hourly sweep drops it. `reason` is the report reason
--    byte the moderator's signed statement commits to, `reason_salt` the salt
--    of that commitment, `statement_id` and `log_index` name the statement in
--    the moderation log. The appeal is the person's text sealed to the
--    moderator list (`lakko_moderation::appeal`); auth stores it and never
--    opens it. `appeal_state` 0 none, 1 sent, 2 declined.
--
-- 2. `moderation_applied_statements`: the statement ids auth applied, so a
--    replayed statement is answered as a repeat and changes nothing.
--
-- 3. `moderation_list_state`: the highest moderator-list version auth has
--    seen; a statement verified under an older list is refused.
--
-- 4. An index on `accounts.identity_pubkey`: a statement names an account
--    by its identity key.

CREATE TABLE IF NOT EXISTS account_suspensions (
    account_id      BINARY(16)       NOT NULL COMMENT 'FK -> accounts.id',
    until_ms        BIGINT           NULL     COMMENT 'unix ms the suspension ends; NULL = no end',
    reason          TINYINT UNSIGNED NOT NULL COMMENT 'report reason byte (lakko_moderation::Reason)',
    reason_salt     BINARY(16)       NOT NULL COMMENT 'salt of the statement''s reason commitment',
    statement_id    BINARY(16)       NOT NULL COMMENT 'the moderator statement that suspended',
    log_index       BIGINT UNSIGNED  NOT NULL COMMENT 'its index in the moderation log',
    suspended_at_ms BIGINT           NOT NULL COMMENT 'unix ms auth applied it',
    appeal_state    TINYINT UNSIGNED NOT NULL DEFAULT 0 COMMENT '0 none, 1 sent, 2 declined',
    appeal          MEDIUMBLOB       NULL     COMMENT 'the appeal sealed to the moderators; opaque here',
    appeal_at_ms    BIGINT           NULL     COMMENT 'unix ms the appeal arrived',
    PRIMARY KEY (account_id),
    CONSTRAINT fk_account_suspensions_account
        FOREIGN KEY (account_id) REFERENCES accounts (id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- The sweep deletes by end time.
CREATE INDEX idx_account_suspensions_until ON account_suspensions (until_ms);

CREATE TABLE IF NOT EXISTS moderation_applied_statements (
    statement_id  BINARY(16) NOT NULL,
    applied_at_ms BIGINT     NOT NULL COMMENT 'unix ms auth applied it',
    PRIMARY KEY (statement_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- The issue time of the newest moderator decision applied to each account,
-- kept after a lift deletes the suspension row, so a Suspend resent after a
-- later Lift (or a Lift after a later Suspend) changes nothing.
CREATE TABLE IF NOT EXISTS moderation_account_decisions (
    account_id   BINARY(16) NOT NULL COMMENT 'FK -> accounts.id',
    issued_at_ms BIGINT UNSIGNED NOT NULL COMMENT 'issued_at_ms of the newest statement applied',
    PRIMARY KEY (account_id),
    CONSTRAINT fk_moderation_account_decisions_account
        FOREIGN KEY (account_id) REFERENCES accounts (id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS moderation_list_state (
    id           TINYINT UNSIGNED NOT NULL,
    list_version BIGINT UNSIGNED  NOT NULL COMMENT 'the highest moderator-list version seen',
    PRIMARY KEY (id),
    CONSTRAINT ck_moderation_list_state_one_row CHECK (id = 1)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE INDEX idx_accounts_identity_pubkey ON accounts (identity_pubkey);
