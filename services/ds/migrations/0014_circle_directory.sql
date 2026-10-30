-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0086: a person's circle directory. The server keeps ciphertext and one
-- read key, and parses none of it.
--
-- `read_pk` is the only read key a directory has: registering the next one
-- replaces it, so a read under the previous epoch is refused from that commit
-- on. `version` is the owner's compare-and-swap: every write names the version
-- it creates. `write_hash` is the digest of the last write's body, so a write
-- that lost its answer can be sent again without being applied twice.
CREATE TABLE IF NOT EXISTS ds_directory (
    directory_id  BINARY(32)      NOT NULL,
    owner_pk      BINARY(32)      NOT NULL,
    epoch         BIGINT UNSIGNED NOT NULL,
    read_pk       BINARY(32)      NOT NULL,
    version       BIGINT UNSIGNED NOT NULL,
    head          VARBINARY(64)   NOT NULL,
    write_hash    BINARY(32)      NOT NULL,
    created_at    TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (directory_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- One optional edge per tree node, public: a wrapped key only a holder of the
-- node's key opens. Addressed by number so a member reads its own path, about
-- half a kilobyte, and the server need not understand a tree to serve it.
CREATE TABLE IF NOT EXISTS ds_directory_edges (
    directory_id  BINARY(32)      NOT NULL,
    node          INT UNSIGNED    NOT NULL,
    wrap          VARBINARY(64)   NOT NULL,
    PRIMARY KEY (directory_id, node),
    CONSTRAINT fk_directory_edges
        FOREIGN KEY (directory_id) REFERENCES ds_directory(directory_id)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- The chain of older epoch keys, public: each epoch wraps the one before it.
CREATE TABLE IF NOT EXISTS ds_directory_chain (
    directory_id  BINARY(32)      NOT NULL,
    epoch         BIGINT UNSIGNED NOT NULL,
    wrap          VARBINARY(64)   NOT NULL,
    PRIMARY KEY (directory_id, epoch),
    CONSTRAINT fk_directory_chain
        FOREIGN KEY (directory_id) REFERENCES ds_directory(directory_id)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- The sealed filter, in its own table so a head read never touches it.
CREATE TABLE IF NOT EXISTS ds_directory_filter (
    directory_id  BINARY(32)      NOT NULL,
    filter_bytes  MEDIUMBLOB      NOT NULL,
    PRIMARY KEY (directory_id),
    CONSTRAINT fk_directory_filter
        FOREIGN KEY (directory_id) REFERENCES ds_directory(directory_id)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- The sealed rows, one length each.
CREATE TABLE IF NOT EXISTS ds_directory_rows (
    directory_id  BINARY(32)      NOT NULL,
    row_index     INT UNSIGNED    NOT NULL,
    row_bytes     VARBINARY(1024) NOT NULL,
    PRIMARY KEY (directory_id, row_index),
    CONSTRAINT fk_directory_rows
        FOREIGN KEY (directory_id) REFERENCES ds_directory(directory_id)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
