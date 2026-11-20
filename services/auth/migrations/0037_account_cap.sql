-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors
--
-- ADR-0046 amendment of 2026-11-18: the public beta is capped at a number of
-- accounts with a username (`LAKKO_AUTH_MAX_ACCOUNTS`). An account takes a
-- place when it binds its first login handle; the bind counts
-- `login_handles` under this one row's lock, so two first binds at once
-- cannot both take the last place. The row carries nothing else.
CREATE TABLE account_cap_lock (
    id TINYINT UNSIGNED NOT NULL PRIMARY KEY
) ENGINE=InnoDB COMMENT 'one row; serialises first login-handle binds against the account cap';

INSERT INTO account_cap_lock (id) VALUES (1);
