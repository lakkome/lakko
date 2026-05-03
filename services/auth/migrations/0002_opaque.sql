-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 1.1b — OPAQUE-PAKE (RFC draft `opaque`) authentication.
--
-- The server stores only OPAQUE-blinded "envelopes" — never the plaintext
-- password. The static server-side OPRF + AKE keypair is generated once
-- at bootstrap and kept in `opaque_server_setup` (single row).
-- Per-account registration material lives in `opaque_registrations`,
-- keyed by account_id from the `accounts` table.

-- Server-wide static setup (single row, id=1 enforced).
-- Created by the auth service on first start, read-only afterwards.
CREATE TABLE IF NOT EXISTS opaque_server_setup (
    id           TINYINT     NOT NULL PRIMARY KEY DEFAULT 1,
    -- Serialized `ServerSetup<CipherSuite>` from opaque-ke.
    -- Contains: AKE keypair (Ristretto255 / P-256 scalars + points),
    -- OPRF seed, and a fake-credentials keypair. Never leaves the server.
    setup_bytes  VARBINARY(2048) NOT NULL,
    created_at   TIMESTAMP   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT chk_opaque_server_setup_singleton CHECK (id = 1)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Per-account registration record. Once signup/finish completes, we store
-- the OPAQUE envelope + masking_key + client AKE pubkey, all serialized
-- into a single blob. Nothing PII (no password, no username).
CREATE TABLE IF NOT EXISTS opaque_registrations (
    account_id          BINARY(16)      NOT NULL PRIMARY KEY,
    registration_bytes  VARBINARY(4096) NOT NULL,
    created_at          TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP
                                            ON UPDATE CURRENT_TIMESTAMP,
    CONSTRAINT fk_opaque_registrations_account
        FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
