-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors
--
-- ADR-0075 — a recovery email, an account without a saved phrase, and taking
-- an account back with the phrase.
--
-- What the server keeps about an address is `email_key`, a keyed BLAKE3 under
-- `LAKKO_AUTH_EMAIL_PEPPER` over the login handle and the normalised address
-- together. The address itself is never stored and never logged (invariant
-- #1); it is in memory for the length of a request, when a letter is sent.

-- One row per account that added a recovery email.
CREATE TABLE IF NOT EXISTS recovery_emails (
    account_id BINARY(16)   NOT NULL PRIMARY KEY COMMENT 'FK -> accounts.id',
    email_key  BINARY(32)   NOT NULL COMMENT 'keyed BLAKE3 over handle and address',
    created_at TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    CONSTRAINT fk_recovery_emails_account
        FOREIGN KEY (account_id) REFERENCES accounts (id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- The email vault of an account whose owner has not saved a recovery phrase:
-- the 44-byte blob the client sealed (the 16-byte phrase entropy under a key
-- Argon2id-derived from the login handle, the address and `vault_salt`),
-- wrapped again at rest under LAKKO_VAULT_ENC_KEY like `passkey_vaults`.
-- Released only in the answer to a verified mailed code (and authenticator
-- code). This table is the stated exception to invariant #2 of ADR-0075
-- section 2: an operator who records the address can open the blob.
CREATE TABLE IF NOT EXISTS email_vaults (
    account_id BINARY(16)     NOT NULL PRIMARY KEY COMMENT 'FK -> accounts.id',
    vault_salt BINARY(16)     NOT NULL COMMENT 'server-minted at enrol start',
    body_enc   VARBINARY(255) NOT NULL COMMENT 'seal_with_key(vault_enc_key, sealed_entropy)',
    created_at TIMESTAMP(3)   NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    CONSTRAINT fk_email_vaults_account
        FOREIGN KEY (account_id) REFERENCES accounts (id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- One mailed code. `purpose` 1 = enrol, 2 = recover. One live challenge per
-- (account, purpose): a new start replaces the previous one. The code is
-- stored as a keyed hash; ten minutes, five tries.
--
-- No foreign key on `account_id`: a recover start that does not match (an
-- unknown login handle, an account without a recovery email, another address)
-- files a decoy row under a stand-in id, so a match and a miss do the same
-- writes. A decoy's `code_key` is random and verifies for no code. Rows live
-- ten minutes and are pruned.
CREATE TABLE IF NOT EXISTS email_challenges (
    id            BINARY(16)       NOT NULL PRIMARY KEY COMMENT 'random; the wire challenge_id',
    account_id    BINARY(16)       NOT NULL COMMENT 'the account, or a stand-in for a decoy',
    purpose       TINYINT UNSIGNED NOT NULL COMMENT '1 enrol, 2 recover',
    code_key      BINARY(32)       NOT NULL COMMENT 'keyed BLAKE3 over id and code',
    pending_key   BINARY(32)       NULL COMMENT 'enrol: the email_key being confirmed',
    vault_salt    BINARY(16)       NULL COMMENT 'enrol: the salt handed to the client',
    handle_tag    BINARY(32)       NOT NULL COMMENT 'keyed hash of the login handle; names the verify counter',
    attempts      INT UNSIGNED     NOT NULL DEFAULT 0,
    expires_at_ms BIGINT           NOT NULL,
    UNIQUE KEY uq_email_challenges_account_purpose (account_id, purpose),
    INDEX idx_email_challenges_expires (expires_at_ms)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- What a verified recover code turns into: a single-use ticket, stored as a
-- keyed hash, ten minutes. `totp_required` is fixed when the ticket is minted;
-- neither the vault nor the password routes proceed until `totp_passed`.
-- `password_mode` is set by `recover/open` when it answers the password mode;
-- the password routes require it.
CREATE TABLE IF NOT EXISTS email_tickets (
    ticket_key    BINARY(32)   NOT NULL PRIMARY KEY COMMENT 'keyed BLAKE3 of the ticket',
    account_id    BINARY(16)   NOT NULL COMMENT 'FK -> accounts.id',
    totp_required TINYINT(1)   NOT NULL,
    totp_passed   TINYINT(1)   NOT NULL DEFAULT 0,
    totp_attempts INT UNSIGNED NOT NULL DEFAULT 0,
    password_mode TINYINT(1)   NOT NULL DEFAULT 0,
    expires_at_ms BIGINT       NOT NULL,
    INDEX idx_email_tickets_account (account_id),
    INDEX idx_email_tickets_expires (expires_at_ms),
    CONSTRAINT fk_email_tickets_account
        FOREIGN KEY (account_id) REFERENCES accounts (id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- What the account's own devices are told (ADR-0075 section 9). `kind`:
-- 1 password changed by email, 2 account opened by email (vault released),
-- 3 recovery email added, 4 replaced, 5 removed, 6 recovery phrase saved.
-- No address, no device detail. Pruned after 60 days.
CREATE TABLE IF NOT EXISTS account_notices (
    id         BIGINT UNSIGNED  NOT NULL AUTO_INCREMENT PRIMARY KEY,
    account_id BINARY(16)       NOT NULL COMMENT 'FK -> accounts.id',
    kind       TINYINT UNSIGNED NOT NULL,
    at_ms      BIGINT           NOT NULL,
    INDEX idx_account_notices_account (account_id, id),
    INDEX idx_account_notices_at (at_ms),
    CONSTRAINT fk_account_notices_account
        FOREIGN KEY (account_id) REFERENCES accounts (id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Whether the owner has saved a recovery phrase (ADR-0075 section 1). Set by
-- `DELETE /recovery-email/vault` and by a key rotation that says so; never
-- cleared. An account with it set never gets an email vault.
ALTER TABLE accounts
    ADD COLUMN phrase_saved TINYINT(1) NOT NULL DEFAULT 0
        COMMENT 'ADR-0075: the owner saved a recovery phrase';

-- The recall's phrase arm (ADR-0075 section 8): an anchor also keeps the
-- recovery envelope in force at capture, and whether the account had an email
-- vault then. With a vault, whoever recovers by mail holds the same phrase as
-- the owner, so the phrase proves nothing and the arm is refused.
ALTER TABLE recall_anchors
    ADD COLUMN recovery_registration VARBINARY(4096) NULL
        COMMENT 'OPAQUE recovery registration in force at capture',
    ADD COLUMN mail_protected TINYINT(1) NOT NULL DEFAULT 0
        COMMENT 'the account had an email vault at capture';
