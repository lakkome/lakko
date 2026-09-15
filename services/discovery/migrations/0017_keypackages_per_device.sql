-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors
--
-- ADR-0050: one KeyPackage per (account, device).
--
-- Every conversation leaf is now a per-device identity, so a contact that
-- adds an account adds every one of its devices, and the directory must
-- serve one KeyPackage per device rather than one per account. `device_id`
-- is the device leaf's 32-byte Ed25519 signature key. The server never
-- parses the bytes; the peer verifies the leaf's device binding (the
-- account-key-signed statement in the leaf credential) before adding it.
--
-- No deployment holds data worth migrating (docs/PRODUCT-READINESS.md), so
-- the table is recreated rather than altered; every client re-publishes on
-- its next sign-in.

DROP TABLE IF EXISTS keypackages;

CREATE TABLE keypackages (
    account_id        BINARY(16)      NOT NULL
                        COMMENT 'UUIDv7 raw bytes — matches services/auth.accounts.id',
    device_id         BINARY(32)      NOT NULL
                        COMMENT 'The device leaf Ed25519 signature key (ADR-0050)',
    keypackage_bytes  VARBINARY(2048) NOT NULL
                        COMMENT 'TLS-encoded MLS KeyPackage (RFC 9420 section 10)',
    created_at        TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at        TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP
                        ON UPDATE CURRENT_TIMESTAMP,
    PRIMARY KEY (account_id, device_id),
    INDEX idx_keypackages_account (account_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
