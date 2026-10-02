-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- `login_handles` is keyed on the account, not on the handle.
--
-- With `handle_key` as the primary key and a unique index on `account_id`
-- (migration 0021), two accounts binding at the same time could deadlock
-- (MariaDB 1213), in two ways, both through the `account_id` index, whose
-- entries for new accounts are neighbours because account ids are UUIDv7:
--
--   1. a first bind ran `DELETE ... WHERE account_id = ?` for a row that
--      does not exist, which takes a gap lock; two first binds held the same
--      gap and each waited for the other to insert into it;
--   2. a change of handle deleted and re-inserted the account's row, and the
--      duplicate-key check on the unique `account_id` index takes next-key
--      locks that reach the neighbouring account's entry.
--
-- Keyed on `account_id`, a first bind is a plain insert, a change is an
-- UPDATE of one row found by primary key, and the only unique check that
-- scans is on `handle_key`, whose values are 32 random-looking bytes and are
-- not neighbours of anything. The lookup by handle (`resolve`) uses the
-- unique index and is unchanged.
--
-- The foreign key on `account_id` is served by the new primary key, so it
-- stays in place through the change.

ALTER TABLE login_handles
    DROP PRIMARY KEY,
    DROP INDEX uq_login_handles_account,
    ADD PRIMARY KEY (account_id),
    ADD UNIQUE KEY uq_login_handles_handle (handle_key);
