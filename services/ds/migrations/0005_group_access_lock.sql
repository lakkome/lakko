-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0053, the security pass: registrations of one group serialize.
--
-- `PUT /ds/group/<hex>/access` reads the group's registered epochs, applies
-- the epoch rule and writes, and two concurrent registrations used to decide
-- against the same read (two first claims of one journal both succeeded, the
-- last writer's key winning). Each registration transaction now starts by
-- upserting this group's row here, which holds an exclusive record lock until
-- the transaction ends. A record lock on a primary-key row locks that row
-- only - no gap - so registrations of DIFFERENT groups never wait on each
-- other, which a locking read of `ds_group_access` (whose empty-range locks
-- are gap locks shared with neighbouring groups) could not promise.
--
-- One row per group ever registered, carrying nothing but the id the access
-- table already holds.
CREATE TABLE IF NOT EXISTS ds_group_access_lock (
    group_id    BINARY(32)      NOT NULL,
    PRIMARY KEY (group_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
