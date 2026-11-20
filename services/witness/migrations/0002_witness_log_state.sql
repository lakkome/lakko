-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Witness v2 (ADR-0098 section 5): one witness key co-signs several logs, so
-- the high-water is kept per log. `log_id` is `lakko_transparency::LogId`'s
-- byte (1 = KT, 2 = moderation). No production data exists: the old single-row
-- table is dropped without a copy (flag day).

DROP TABLE IF EXISTS witness_state;

CREATE TABLE witness_log_state (
    log_id    TINYINT UNSIGNED NOT NULL PRIMARY KEY,
    tree_root BINARY(32)       NOT NULL,
    log_size  BIGINT UNSIGNED  NOT NULL
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4 COLLATE = utf8mb4_unicode_ci;
