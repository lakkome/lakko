-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors
--
-- ADR-0057: a journal has an owner; only those who change a group register
-- its epochs.
--
-- Flag day (no production data): the ADR-0053 epoch table is dropped and
-- recreated with who registered each epoch, and three tables join it. The
-- per-group lock row of migration 0005 stays and now serializes every
-- ownership statement of a group, not only registrations.
--
-- Everything here is a PUBLIC key, a rank, or a keyed hash: invariants #2
-- and #5 (every key is derived per group, so none links two groups or names
-- an account).

DROP TABLE IF EXISTS ds_group_access;

-- One row per claimed journal. A journal with no row here is unclaimed and
-- keeps the Phase 1.3 open posture.
CREATE TABLE IF NOT EXISTS ds_group_owner (
    group_id    BINARY(32)      NOT NULL,
    -- Ed25519 verifying key of the journal's root (derived by the owner's
    -- account from its identity seed and this group id).
    root_pk     BINARY(32)      NOT NULL,
    created_at  TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    updated_at  TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (group_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- The epoch chain: the two newest write, the retained tail reads (ADR-0053).
-- `registered_by` / `registered_rank` record who put each row there, so a
-- higher rank can replace what a lower one registered (3 = the root,
-- 2 = admin, 1 = member).
CREATE TABLE IF NOT EXISTS ds_group_access (
    group_id         BINARY(32)       NOT NULL,
    epoch            BIGINT UNSIGNED  NOT NULL,
    access_pk        BINARY(32)       NOT NULL,
    registered_by    BINARY(32)       NOT NULL,
    registered_rank  TINYINT UNSIGNED NOT NULL,
    created_at       TIMESTAMP(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (group_id, epoch)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- The registrars the root appointed (1 = member, 2 = admin).
CREATE TABLE IF NOT EXISTS ds_group_registrars (
    group_id        BINARY(32)       NOT NULL,
    registrar_pk    BINARY(32)       NOT NULL,
    registrar_rank  TINYINT UNSIGNED NOT NULL,
    created_at      TIMESTAMP(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (group_id, registrar_pk)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- A journal's push binding: keyed-BLAKE3(LAKKO_DS_BINDING_PEPPER, push_gid).
-- The unique key makes one push_gid belong to at most one journal; a wake
-- whose push_gid does not hash to the publishing journal's row is dropped.
CREATE TABLE IF NOT EXISTS ds_push_bindings (
    group_id     BINARY(32)      NOT NULL,
    binding_key  BINARY(32)      NOT NULL,
    created_at   TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (group_id),
    UNIQUE KEY uq_push_binding_key (binding_key)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
