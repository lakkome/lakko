-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0068 section 1: a username can be replaced.
--
-- A row of `usernames` is either CURRENT (`retired_at IS NULL`) or RETIRED.
-- A retired row is the name an account held before a rename: no lookup and no
-- name search answers for it, its `is_public` is cleared, and it still
-- occupies its hash, so nobody else can register it until the pruner deletes
-- it (30 days after `retired_at` by default). The account that retired it may
-- take it back.
--
-- An account holds at most ONE current name. Until now nothing enforced
-- that: a second `register` added a second row. Any such extra rows are
-- deleted here (the smallest hash is kept; there is no deployment whose data
-- this could matter to), and the unique key below makes the rule structural:
-- `current_account` is the account id of a current row and NULL for a
-- retired one, and a unique key admits any number of NULLs.

DELETE u1 FROM usernames u1
  JOIN usernames u2
    ON u1.account_id = u2.account_id AND u1.oprf_hash > u2.oprf_hash;

ALTER TABLE usernames
    ADD COLUMN retired_at TIMESTAMP(3) NULL DEFAULT NULL
        COMMENT 'ADR-0068: set when the account renamed away from this name',
    ADD COLUMN current_account BINARY(16)
        AS (IF(retired_at IS NULL, account_id, NULL)) PERSISTENT
        COMMENT 'account_id of a current row, NULL of a retired one',
    ADD UNIQUE KEY uq_usernames_current_account (current_account),
    ADD INDEX idx_usernames_retired (retired_at);

-- When an account last renamed (ADR-0068 D2: one rename per 30 days). One
-- row per account that ever renamed; deleted with the account.
CREATE TABLE IF NOT EXISTS username_renames (
    account_id      BINARY(16)   NOT NULL PRIMARY KEY,
    last_rename_at  TIMESTAMP(3) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
