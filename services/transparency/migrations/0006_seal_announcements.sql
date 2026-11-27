-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0101 section 10: the seal announcement as a leaf of its own kind, and
-- the reset seal as a sidecar of a reset leaf.
--
-- `log_announcements` holds the announcement leaves. They share the
-- 0-based index sequence with `log_entries` (the replay at boot merges the
-- two tables by `idx`), and like those rows they are APPEND-ONLY: never
-- updated, never deleted. The Merkle leaf is
-- `lakko_transparency::AnnouncementLeaf::canonical_bytes` over
-- `(username_hash, key_generation, seq, announcement_hash)`; the
-- announcement bytes are kept beside it so a client can fetch the newest
-- announcement of a generation (`GET /transparency/announcement/...`).
--
-- `log_entries.reset_seal` is the 214-byte reset seal discovery forwards
-- with a reset leaf: opaque to the tree (never part of `canonical_bytes`),
-- served by `/transparency/latest`. Nullable: an unsealed reset, or no reset.

CREATE TABLE IF NOT EXISTS log_announcements (
    idx               BIGINT UNSIGNED NOT NULL PRIMARY KEY,
    username_hash     BINARY(32)      NOT NULL,
    key_generation    BIGINT UNSIGNED NOT NULL,
    seq               BIGINT UNSIGNED NOT NULL,
    announcement_hash BINARY(32)      NOT NULL,
    announcement      VARBINARY(641)  NOT NULL,
    created_at        TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    INDEX idx_announcement_generation (username_hash, key_generation, seq),
    UNIQUE KEY uq_announcement_hash (username_hash, announcement_hash)
) ENGINE = InnoDB
  DEFAULT CHARSET = utf8mb4
  COLLATE = utf8mb4_unicode_ci;

ALTER TABLE log_entries
    ADD COLUMN reset_seal VARBINARY(214) NULL
    COMMENT 'ADR-0101: the 214-byte reset seal of a reset leaf; opaque sidecar';
