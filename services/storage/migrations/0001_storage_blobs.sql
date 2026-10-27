-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0065 section 1 - the blob index of the storage service.
--
-- One row per content-addressed attachment blob. Every blob request is
-- answered from this table; the file under LAKKO_BLOB_DIR holds the bytes.
-- Nothing in a row names an account (invariant #5): the id is the SHA-256 of
-- the ciphertext, the delete key is a per-blob Ed25519 public key derived on
-- the client from the account seed and the blob id (ADR-0065 section 6), and
-- the two day columns are day numbers (unix seconds / 86400), so the server
-- keeps no record of when a file is read finer than the day.
CREATE TABLE IF NOT EXISTS storage_blobs (
    blob_id          BINARY(32)      NOT NULL COMMENT 'SHA-256 of the ciphertext',
    size_bytes       BIGINT UNSIGNED NOT NULL,
    node             VARCHAR(64)     NOT NULL COMMENT 'the storage node holding the file',
    created_day      INT UNSIGNED    NOT NULL COMMENT 'unix day of the first upload or the import',
    last_access_day  INT UNSIGNED    NOT NULL COMMENT 'unix day of the last download, keep or re-upload',
    delete_pk        BINARY(32)      NULL     COMMENT 'Ed25519 key DELETE must be signed with; NULL = undeletable',
    PRIMARY KEY (blob_id),
    KEY idx_storage_blobs_last_access (last_access_day)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- The one-time import of blob files that existed before the index: one row
-- per node once it has run.
CREATE TABLE IF NOT EXISTS storage_blob_imports (
    node      VARCHAR(64)  NOT NULL,
    done_day  INT UNSIGNED NOT NULL,
    PRIMARY KEY (node)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
