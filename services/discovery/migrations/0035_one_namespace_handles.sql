-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0087 sections 1, 2 and 4: one namespace for people, groups and
-- channels; groups and channels hold their @name with a handle key that
-- names no account; a public one is published like a person in Everyone.
--
-- A flag day: nothing is deployed, so the channel namespace, the person-only
-- record table and the person-only term table are dropped rather than
-- migrated. Test stacks are reset.

-- The uniqueness table. `usernames` and `handles` each insert here in the
-- transaction that takes a name, so a person and a group can never hold one
-- name. A row lives as long as its owner row, retired rows included (the
-- 30-day hold). kind: 1 a person's username, 2 a group's or channel's handle.
CREATE TABLE IF NOT EXISTS name_claims (
    lookup_key  BINARY(64)       NOT NULL PRIMARY KEY,
    kind        TINYINT UNSIGNED NOT NULL,
    created_at  TIMESTAMP(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

INSERT IGNORE INTO name_claims (lookup_key, kind) SELECT oprf_hash, 1 FROM usernames;

-- `@channel` handles had their own OPRF domain and table (ADR-0059 G4-5);
-- ADR-0087 section 1 puts every name in one namespace.
DROP TABLE IF EXISTS channel_names;

-- One row per name a group or channel holds or recently held. No account,
-- no group or channel id: `handle_id` is random and is what a public record
-- and its search terms are keyed on. `write_pk` is the handle key every
-- statement is signed by. A row is CURRENT (`retired_at IS NULL`) or
-- RETIRED (renamed away, deleted, or lapsed when `lease_until` passed); a
-- retired row answers nothing and still holds its name for the hold. One
-- current row per handle key (`current_write_pk`, a unique key admitting any
-- number of NULLs).
CREATE TABLE IF NOT EXISTS handles (
    lookup_key        BINARY(64)   NOT NULL PRIMARY KEY,
    handle_id         BINARY(16)   NOT NULL,
    write_pk          BINARY(32)   NOT NULL,
    is_public         BOOLEAN      NOT NULL DEFAULT FALSE,
    -- An explicit default, so MariaDB never adds ON UPDATE to it.
    lease_until       TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    retired_at        TIMESTAMP(3) NULL DEFAULT NULL,
    current_write_pk  BINARY(32)
        AS (IF(retired_at IS NULL, write_pk, NULL)) PERSISTENT,
    created_at        TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    UNIQUE KEY uq_handles_handle_id (handle_id),
    UNIQUE KEY uq_handles_current_write_pk (current_write_pk),
    INDEX idx_handles_write_pk (write_pk),
    INDEX idx_handles_lease (lease_until),
    INDEX idx_handles_retired (retired_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Public records of people in Everyone (kind 1, target = account id) and of
-- public groups and channels (kind 2, target = handle_id). One query serves
-- both. `record` is at most
-- `lakko_contacts::introduction::PUBLIC_RECORD_MAX_LEN` (7411) bytes;
-- `MAX_PUBLIC_RECORD_LEN` in `src/visibility.rs` derives it from the codec
-- and a test pins it against this file.
DROP TABLE IF EXISTS public_records;
CREATE TABLE public_records (
    kind        TINYINT UNSIGNED NOT NULL,
    target      BINARY(16)       NOT NULL,
    record      VARBINARY(7411)  NOT NULL,
    updated_at  TIMESTAMP(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3)
                                 ON UPDATE CURRENT_TIMESTAMP(3),
    PRIMARY KEY (kind, target)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Name search terms (lookup keys of ADR-0060 name terms) of the same two
-- kinds. A name lookup returns the oldest first (ADR-0087 section 4: a later
-- flood of one prefix cannot push out what is there), so `created_at` of a
-- term survives a re-publish that keeps it.
DROP TABLE IF EXISTS display_names;
CREATE TABLE search_terms (
    lookup_key  BINARY(64)       NOT NULL,
    kind        TINYINT UNSIGNED NOT NULL,
    target      BINARY(16)       NOT NULL,
    created_at  TIMESTAMP(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (lookup_key, kind, target),
    INDEX idx_search_terms_target (kind, target),
    INDEX idx_search_terms_order (lookup_key, created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Nonces of handle statements seen within twice the statement window, so a
-- captured statement cannot be replayed.
CREATE TABLE IF NOT EXISTS handle_statement_nonces (
    nonce    BINARY(16)   NOT NULL PRIMARY KEY,
    seen_at  TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    INDEX idx_handle_statement_nonces_seen (seen_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
