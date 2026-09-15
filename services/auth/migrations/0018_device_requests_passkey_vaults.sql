-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0049 (Identity recovery) — sections 2 and 3.
--
-- device_requests: a fresh device that has no camera to be scanned posts
-- its pairing payload (the exact bytes the pairing QR carries: the
-- device's KeyPackage + the rendezvous pseudonym, public material) here;
-- the account's signed-in devices poll for it, show a comparison code
-- derived from the payload, and on Confirm run the unchanged ADR-0018
-- add-device flow. One open request per account (the PK); a new request
-- replaces the old one so a spammer cannot fill a user's prompt list.
-- Short TTL, never parsed server-side (invariant #2), no social-graph
-- edge (invariant #5: the payload is what the DS would see at rendezvous).
--
-- passkey_vaults: the phrase entropy sealed by the client under a key
-- derived from a passkey's WebAuthn PRF output, one blob per credential,
-- wrapped at rest under LAKKO_VAULT_ENC_KEY like word_vaults. Only the
-- credential's PRF output opens it; the server can never open it, and a
-- synced passkey carries the identity to a new device. Same invariant-#2
-- trust class as word_vaults (ADR-0049 section 6), but with no guess
-- surface at all: there is no verifier and nothing to brute-force
-- without the authenticator.

CREATE TABLE IF NOT EXISTS device_requests (
    account_id       BINARY(16)      NOT NULL PRIMARY KEY,
    request_id       BINARY(16)      NOT NULL,
    -- Opaque pairing payload; the same bytes the QR code would carry.
    pairing_payload  VARBINARY(2048) NOT NULL,
    created_at       TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    expires_at       TIMESTAMP(3)    NOT NULL,
    INDEX idx_device_requests_request (request_id),
    INDEX idx_device_requests_expires (expires_at),
    CONSTRAINT fk_device_requests_account
        FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS passkey_vaults (
    account_id      BINARY(16)     NOT NULL,
    credential_id   VARBINARY(1024) NOT NULL,
    -- seal_with_key(vault_key, sealed_entropy).
    body_enc        VARBINARY(255) NOT NULL,
    created_at      TIMESTAMP(3)   NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (account_id, credential_id(255)),
    CONSTRAINT fk_passkey_vaults_account
        FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
