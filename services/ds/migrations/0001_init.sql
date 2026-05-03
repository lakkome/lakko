-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 1.3 — MLS Delivery Service skeleton.
--
-- Two append-only journals; both ciphertext-only.
--
--   * `ds_group_messages` — per-group fan-out. MLS Commit and
--     ApplicationData messages produced by any member land here, keyed
--     by `group_id` (a 32-byte pseudonym; the server cannot link it to
--     identities). Other members poll with `WHERE group_id = ? AND id > ?
--     ORDER BY id`.
--
--   * `ds_mailbox_messages` — per-device direct mailbox. Used primarily
--     for MLS Welcome messages (addressed to a specific joiner whose
--     KeyPackage was used to encrypt them). The sender knows the
--     recipient's `device_id` pseudonym; the server does not.
--
-- The auto-incrementing `id` doubles as the cursor a client uses to
-- catch up: "give me everything for this group/device after id=N".
-- INSERTs serialize through the AUTO_INCREMENT counter, so per-group
-- ordering is automatic without a per-group sequence column.
--
-- Retention: 30 days default, enforced by a periodic pruner (see
-- `src/journal.rs::spawn_pruner`). A device offline longer than that
-- loses old messages and must rejoin via a fresh Welcome.

CREATE TABLE IF NOT EXISTS ds_group_messages (
    id          BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    group_id    BINARY(32)      NOT NULL,
    -- The MLS ciphertext envelope. LONGBLOB is up to 4 GB, but the
    -- HTTP layer caps individual messages at 1 MB; large attachments
    -- live in `services/storage` and are referenced by hash.
    payload     LONGBLOB        NOT NULL,
    created_at  TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (id),
    -- Hot path: WHERE group_id = ? AND id > ? ORDER BY id LIMIT ?
    INDEX idx_group_id_id (group_id, id),
    -- Pruner path: WHERE created_at < NOW() - INTERVAL ? SECOND
    INDEX idx_created_at (created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS ds_mailbox_messages (
    id          BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    device_id   BINARY(32)      NOT NULL,
    payload     LONGBLOB        NOT NULL,
    created_at  TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (id),
    INDEX idx_device_id_id (device_id, id),
    INDEX idx_created_at (created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
