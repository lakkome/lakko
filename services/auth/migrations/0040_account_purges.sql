-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors
--
-- Account deletion reaches the other services through this outbox.
--
-- `DELETE /account` and the unclaimed-account sweep insert one row per
-- deleted account in the same transaction that deletes the account. A
-- background worker (`src/purge.rs`) then asks `services/discovery` to purge
-- the account's directory rows and `services/storage` to remove its backup
-- blob, over their internal listeners, records each one done, retries with
-- backoff on failure, and deletes the row once both are done.
--
-- No foreign key to `accounts`: the account row is gone by the time this row
-- is read. The row holds the account id and bookkeeping only.
CREATE TABLE account_purges (
    account_id         BINARY(16)      NOT NULL PRIMARY KEY,
    created_at         TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,
    discovery_done_at  TIMESTAMP       NULL,
    storage_done_at    TIMESTAMP       NULL,
    attempts           INT UNSIGNED    NOT NULL DEFAULT 0,
    last_attempt_at    TIMESTAMP       NULL,
    next_attempt_at    TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,
    INDEX idx_account_purges_next (next_attempt_at)
) ENGINE=InnoDB COMMENT 'deleted accounts whose discovery rows and backup blob are still to be purged';
