-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0089 security fix F3: one account per identity key, proven, with
-- history.
--
-- 1. `account_identity_targets`: every account target
--    (`account_target_hash(identity_pubkey)`) an account ever recorded, with
--    the account that recorded it. The primary key makes a target belong to
--    one account forever: a second account asserting it is refused (`409
--    identity-key-taken`). Every writer of `accounts.identity_pubkey` (the
--    genesis mint, the key reset, the key rotation, the recall) inserts the
--    row in the same transaction, after it verified the identity key's own
--    proof over the account id. Suspend, Lift, KeepSuspension and the
--    panel's account read find the account through this table, so a
--    statement naming a key the account had before a change still finds it.
--
--    No foreign key, because some rows outlive their account: deleting an
--    account deletes its rows in the same transaction, unless the account
--    is suspended at that moment. Then its rows are kept (`kept = 1`) until
--    the suspension's end (`kept_until_ms`, NULL = no end = kept for good),
--    so a suspended person cannot delete the account and register the same
--    key on a new one; a new account asserting a kept target is refused
--    while it is kept. The hourly suspension sweep drops kept rows whose end
--    has passed, and a moderator's Lift naming a kept target releases all of
--    that account's kept rows (unless issued before `kept_decided_at_ms`,
--    the newest decision applied to the account before it was deleted).
--
-- 2. `accounts.identity_target` becomes unique (NULL for an account with no
--    key recorded yet; NULLs do not collide).
--
-- No backfill: no deployment holds data (the test stack is reset).

CREATE TABLE IF NOT EXISTS account_identity_targets (
    target      BINARY(32) NOT NULL COMMENT 'account_target_hash(identity_pubkey)',
    account_id  BINARY(16) NOT NULL COMMENT 'the account that recorded it; kept after the account is deleted',
    added_at_ms BIGINT     NOT NULL COMMENT 'unix ms auth recorded it',
    kept        TINYINT(1) NOT NULL DEFAULT 0 COMMENT '1 = the account was deleted while suspended; the row is kept',
    kept_until_ms BIGINT   NULL     COMMENT 'kept rows: the suspension end (unix ms); NULL = no end',
    kept_decided_at_ms BIGINT UNSIGNED NULL COMMENT 'kept rows: issued_at_ms of the newest decision applied before deletion',
    PRIMARY KEY (target)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE INDEX idx_account_identity_targets_account ON account_identity_targets (account_id);
CREATE INDEX idx_account_identity_targets_kept ON account_identity_targets (kept, kept_until_ms);

DROP INDEX idx_accounts_identity_target ON accounts;

CREATE UNIQUE INDEX uq_accounts_identity_target ON accounts (identity_target);
