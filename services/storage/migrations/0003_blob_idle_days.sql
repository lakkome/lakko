-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0087 section 13 - a blob's own idle life.
--
-- An upload may name how many days without access the blob lives, one byte,
-- 1..=255; a channel file sets 45 (30 days of posts and 15 of margin). The
-- sweep then deletes the blob when its last access is more than that many
-- days back, in place of the service's default lifetime (365 days). NULL is
-- the default. Set by the first upload; a repeated upload keeps it, as it
-- keeps the delete key. Names no account and no channel.
ALTER TABLE storage_blobs
    ADD COLUMN idle_days TINYINT UNSIGNED NULL
        COMMENT 'days without access before the sweep deletes the blob; NULL = the default lifetime';
