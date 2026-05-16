-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 2a2 — WebAuthn passkey opt-in.
--
-- Passkey is **additional** to OPAQUE-PAKE, not a replacement. Every
-- account always has a recovery phrase as the last-resort path; the
-- passkey is an auth-convenience binding the user attaches to an
-- existing account after they have signed in via the password flow
-- at least once.
--
-- Two tables:
--   1. `webauthn_credentials` — long-lived public-key material. One
--      row per (credential_id) so a single account can register
--      multiple passkeys (one per device).
--   2. `webauthn_states`      — short-lived in-flight challenge state
--      for the registration / authentication two-step flows. Mirrors
--      `opaque_login_states`. Sweeper deletes expired rows on a timer.

CREATE TABLE IF NOT EXISTS webauthn_credentials (
    -- Authenticator-generated credential id. WebAuthn spec allows up
    -- to 1023 bytes; typical authenticators return 16-256 bytes.
    -- `VARBINARY(255)` keeps the index size manageable for InnoDB.
    credential_id   VARBINARY(255)  NOT NULL PRIMARY KEY,
    account_id      BINARY(16)      NOT NULL,
    -- Serialized `webauthn-rs::Passkey` (CBOR / serde). Opaque to
    -- the application; we round-trip via webauthn-rs.
    passkey_bytes   VARBINARY(2048) NOT NULL,
    -- Friendly label the user assigned at registration. The UI lists
    -- credentials by this label so the user can tell "MacBook Touch
    -- ID" from "YubiKey 5C NFC". Empty allowed.
    nickname        VARCHAR(255)    NOT NULL DEFAULT '',
    created_at      TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,
    -- Updated by `finish_passkey_authentication`. NULL until first
    -- successful auth.
    last_used_at    TIMESTAMP       NULL,
    INDEX idx_webauthn_credentials_account (account_id),
    CONSTRAINT fk_webauthn_credentials_account
        FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Short-lived in-flight challenges. The sweeper deletes any row
-- whose `expires_at` is in the past on a 60-second timer (mirror of
-- `opaque_login_states`). Default TTL: 5 minutes per WebAuthn level-2
-- spec guidance.
CREATE TABLE IF NOT EXISTS webauthn_states (
    state_id      BINARY(16)      NOT NULL PRIMARY KEY,
    -- NULL on the discoverable-authentication path where the user
    -- hasn't told us which account they are yet (the authenticator
    -- supplies the user handle on /finish).
    account_id    BINARY(16)      NULL,
    -- Discriminates the kind of state blob: 0 = `PasskeyRegistration`,
    -- 1 = `PasskeyAuthentication`, 2 = `DiscoverableAuthentication`.
    kind          TINYINT UNSIGNED NOT NULL,
    -- Serialized webauthn-rs state — opaque to the application.
    state_bytes   VARBINARY(4096) NOT NULL,
    expires_at    TIMESTAMP       NOT NULL,
    INDEX idx_webauthn_states_expires (expires_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
