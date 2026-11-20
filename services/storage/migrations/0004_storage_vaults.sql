-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0064 section 1 - the vault index of the storage service.
--
-- One row per vault and one row per vault object. The objects themselves are
-- files under LAKKO_BLOB_DIR/.lakko-vaults/, written once under a name that
-- carries their version and reservation token; this index says which version
-- of each object is current and on which node the vault lives.
--
-- Nothing in a row names an account or a conversation (invariant #5): the
-- vault id is a 32-byte derivation from a client-held vault root, the write
-- key is a per-vault Ed25519 public key, and the object rows hold numbers.

CREATE TABLE IF NOT EXISTS storage_vaults (
    vault_id       BINARY(32)      NOT NULL COMMENT 'client-derived vault name',
    write_pk       BINARY(32)      NOT NULL COMMENT 'Ed25519 key every vault request is signed with',
    node           VARCHAR(64)     NOT NULL COMMENT 'the storage node holding the vault',
    created_day    INT UNSIGNED    NOT NULL COMMENT 'unix day of the claim',
    bytes_used     BIGINT UNSIGNED NOT NULL DEFAULT 0 COMMENT 'bytes of committed objects',
    window_hour    BIGINT UNSIGNED NOT NULL DEFAULT 0 COMMENT 'unix hour of the write-rate window',
    window_writes  INT UNSIGNED    NOT NULL DEFAULT 0 COMMENT 'reservations in that hour',
    PRIMARY KEY (vault_id),
    KEY idx_storage_vaults_node (node)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- kind: 1 head (chunk_no 0), 2 chunk. version 0 = nothing committed yet (a
-- row that holds only a reservation). A reservation holds reserved_version
-- until reserved_until_ms; after that the next writer may take it.
CREATE TABLE IF NOT EXISTS storage_vault_objects (
    vault_id           BINARY(32)       NOT NULL,
    kind               TINYINT UNSIGNED NOT NULL,
    chunk_no           INT UNSIGNED     NOT NULL,
    version            BIGINT UNSIGNED  NOT NULL DEFAULT 0,
    token              BINARY(16)       NULL     COMMENT 'reservation that wrote the current version',
    size_bytes         BIGINT UNSIGNED  NOT NULL DEFAULT 0,
    reserved_version   BIGINT UNSIGNED  NULL,
    reserved_token     BINARY(16)       NULL,
    reserved_size      BIGINT UNSIGNED  NOT NULL DEFAULT 0,
    reserved_until_ms  BIGINT UNSIGNED  NOT NULL DEFAULT 0,
    PRIMARY KEY (vault_id, kind, chunk_no),
    CONSTRAINT fk_storage_vault_objects_vault FOREIGN KEY (vault_id)
        REFERENCES storage_vaults (vault_id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
