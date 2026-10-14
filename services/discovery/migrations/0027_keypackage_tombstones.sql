-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0066 sprint T1 (section 2.2): tombstones for deleted device ids.
--
-- Before this, `DELETE /discovery/keypackage/device/{id}` was a plain delete
-- and the directory kept no record of it, so a removed device that still held
-- a session could publish its KeyPackage again and be re-admitted to
-- conversations by any client that had not seen the removal.
--
-- Every delete of one device's row now writes a row here, and
-- `POST /discovery/keypackage` refuses a tombstoned device id
-- (`403 device-revoked`) for the retention window (30 days, the refresh
-- credential's lifetime: every session the removed device could hold has
-- expired by then). The pruner drops older rows.
--
-- An attested key change (ADR-0065: a reset; ADR-0066: a recall or a
-- rotation) deletes the account's tombstones together with its KeyPackage
-- rows, and so does the account erasure.
--
-- What is stored: device ids the directory itself published until they were
-- deleted, keyed on the account id. No key material, no PII.

CREATE TABLE IF NOT EXISTS keypackage_tombstones (
    account_id    BINARY(16) NOT NULL
                    COMMENT 'UUIDv7 raw bytes — matches services/auth.accounts.id',
    device_id     BINARY(32) NOT NULL
                    COMMENT 'The deleted device leaf Ed25519 signature key (ADR-0050)',
    deleted_at_ms BIGINT     NOT NULL
                    COMMENT 'unix ms of the latest delete of this device id',
    PRIMARY KEY (account_id, device_id),
    INDEX idx_keypackage_tombstones_deleted_at (deleted_at_ms)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
