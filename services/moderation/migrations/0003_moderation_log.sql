-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0098 section 5 - the moderation log: an append-only Merkle log of
-- every moderator statement and every moderator list, signed by this
-- service's own operator key and co-signed by the witnesses (witness v2,
-- log `moderation`).
--
-- A leaf is `kind(1) ‖ bytes`: kind 1 a signed statement (the reason only as
-- its salted commitment), kind 2 a root-signed moderator list, kind 3
-- reserved for the suspension snapshot root. A leaf never holds report
-- content or anything about a reporter.

-- The leaves, in order. `dedup_key` makes an append idempotent: a
-- statement's 16-byte id padded with zeros to 32 bytes, or BLAKE3 of a
-- list's bytes.
CREATE TABLE IF NOT EXISTS moderation_log_leaves (
    idx             BIGINT UNSIGNED  NOT NULL,
    kind            TINYINT UNSIGNED NOT NULL,
    dedup_key       BINARY(32)       NOT NULL,
    leaf            MEDIUMBLOB       NOT NULL COMMENT 'kind byte followed by the bytes',
    appended_at_ms  BIGINT UNSIGNED  NOT NULL,
    PRIMARY KEY (idx),
    UNIQUE KEY uq_moderation_log_dedup (dedup_key)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- The log's operator signing key: a 32-byte Ed25519 seed generated on first
-- boot. Single row.
CREATE TABLE IF NOT EXISTS moderation_log_key (
    id    TINYINT UNSIGNED NOT NULL PRIMARY KEY CHECK (id = 1),
    seed  BINARY(32)       NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- The last witness-co-signed head and its co-signature bundle (the
-- concatenated 105-byte co-signatures). Single row.
CREATE TABLE IF NOT EXISTS moderation_log_witnessed (
    id         TINYINT UNSIGNED NOT NULL PRIMARY KEY CHECK (id = 1),
    tree_root  BINARY(32)       NOT NULL,
    log_size   BIGINT UNSIGNED  NOT NULL,
    cosigs     VARBINARY(8192)  NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Per-witness high-water: the size each configured witness (by its position
-- in LAKKO_MODERATION_WITNESS_URLS) last co-signed, so each is handed a
-- consistency proof from its own position.
CREATE TABLE IF NOT EXISTS moderation_log_witness_high_water (
    witness_index  INT UNSIGNED     NOT NULL PRIMARY KEY,
    tree_root      BINARY(32)       NOT NULL,
    log_size       BIGINT UNSIGNED  NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
