-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0099 step 5: a moderator deletes a blob named in a report's evidence
-- by a signed statement, forwarded by the moderation service and checked
-- here against the moderator list under the pinned moderation root
-- (src/moderation.rs).
--
-- 1. `storage_blob_tombstones`: a blob a moderator deleted. Its row and file
--    go; an upload of the id answers `410 blob-removed` from then on, and a
--    read `404 blob-not-found` as for any gone blob. Kept forever: ids are
--    hashes of ciphertext and say nothing about anyone (chosen).
--
-- 2. `moderation_applied_statements`, `moderation_list_state`: the statement
--    ids applied (a replay is a repeat) and the highest moderator-list
--    version seen (an older one is refused).

CREATE TABLE IF NOT EXISTS storage_blob_tombstones (
    blob_id  BINARY(32)   NOT NULL COMMENT 'SHA-256 of the ciphertext',
    at_day   INT UNSIGNED NOT NULL COMMENT 'unix day the moderator statement was applied',
    PRIMARY KEY (blob_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS moderation_applied_statements (
    statement_id  BINARY(16)      NOT NULL,
    applied_at_ms BIGINT UNSIGNED NOT NULL COMMENT 'unix ms storage applied it',
    PRIMARY KEY (statement_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS moderation_list_state (
    id           TINYINT UNSIGNED NOT NULL,
    list_version BIGINT UNSIGNED  NOT NULL COMMENT 'the highest moderator-list version seen',
    PRIMARY KEY (id),
    CONSTRAINT ck_moderation_list_state_one_row CHECK (id = 1)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
