-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 1.1b — OPAQUE-PAKE (RFC draft `opaque`) authentication.
--
-- Server stores только OPAQUE-blinded "envelopes" — никогда plaintext
-- password. Server static OPRF + AKE keypair генерится один раз при
-- bootstrap и хранится в `opaque_server_setup` (single-row).
-- Per-account registration material — в `opaque_registrations`,
-- key-ed по account_id из таблицы `accounts`.

-- Server-wide static setup (single row, id=1 forced).
-- Создаётся auth-service-ом на первом старте, потом read-only.
CREATE TABLE IF NOT EXISTS opaque_server_setup (
    id           TINYINT     NOT NULL PRIMARY KEY DEFAULT 1,
    -- Сериализованный ServerSetup<CipherSuite> из opaque-ke.
    -- Содержит: AKE keypair (Ristretto255 P256 scalars + points),
    -- OPRF seed, fake-credential keypair. Никогда не уходит с сервера.
    setup_bytes  VARBINARY(2048) NOT NULL,
    created_at   TIMESTAMP   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT chk_opaque_server_setup_singleton CHECK (id = 1)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Per-account registration record. После завершения signup/finish здесь
-- лежит OPAQUE envelope + masking_key + client AKE pubkey, всё
-- сериализованное в одном blob-е. Ничего PII (no password, no username).
CREATE TABLE IF NOT EXISTS opaque_registrations (
    account_id          BINARY(16)      NOT NULL PRIMARY KEY,
    registration_bytes  VARBINARY(4096) NOT NULL,
    created_at          TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP
                                            ON UPDATE CURRENT_TIMESTAMP,
    CONSTRAINT fk_opaque_registrations_account
        FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
