-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0018 (Recovery factor cascade, factor (d)) — phone / email
-- one-time-code recovery.
--
-- Two tables back the weakest (last-in-cascade) ACCESS-recovery factor:
--
--   * `recovery_contacts` — one CONFIRMED contact per (account, kind). It
--     records that an account opted into email- or phone-OTP recovery and
--     proved control of the contact at enrollment time.
--   * `otp_challenges`    — at most one live one-time code per (account,
--     kind). A new send UPSERTs (overwrites the prior code + resets the
--     attempt counter); the row is DELETEd on a successful verify and is
--     swept on expiry by the existing state pruner.
--
-- INVARIANT #1 IS LOAD-BEARING: the phone / email PLAINTEXT is NEVER stored
-- and NEVER logged. Only a keyed hash (`contact_hash`) is persisted. The
-- plaintext is supplied transiently at enroll and at recover — used only to
-- address the outgoing message (SMTP / SMS), then dropped. There is no
-- column anywhere in this schema that can hold the raw phone number or email
-- address.
--
-- TRUST CLASS: `contact_hash` is an ACCESS verifier in the same trust class
-- as the OPAQUE password envelope and the TOTP secret — it authenticates a
-- recovery request, it never decrypts E2EE data. Holding it does NOT violate
-- invariant #2 (no message / chain / root / media keys are involved); the
-- classification is recorded in ADR-0018 "Server-held verifiers are ACCESS
-- verifiers".
--
-- HASH CONSTRUCTION: `contact_hash = BLAKE3_keyed(pepper, "lakko/recovery-
-- contact/v1" || kind_byte || normalized_contact)`. The pepper is the
-- server-static `LAKKO_RECOVERY_CONTACT_PEPPER` (32 bytes). Keying the hash
-- on a server secret makes it (a) non-reversible without the secret and (b)
-- un-enumerable: an attacker with the database alone cannot dictionary-attack
-- the hashes back to the contacts, and cannot test whether a chosen
-- phone/email is registered, because computing a candidate hash needs the
-- pepper. This defends a DB-only leak (dump / backup theft). It does NOT
-- defend a full-host compromise (the pepper lives in the service env); that
-- residual is inherent to any server-held contact verifier.
--
-- BRUTE-FORCE STATE: `otp_challenges.attempts` caps verification tries (>= 5
-- -> rejected) and `expires_at` (10-minute TTL) bounds a code's lifetime, so
-- a 6-digit code cannot be exhaustively guessed within its window.

CREATE TABLE IF NOT EXISTS recovery_contacts (
    account_id   BINARY(16)       NOT NULL,
    -- 1 = email, 2 = phone. Wire-format-stable: never renumber.
    kind         TINYINT UNSIGNED NOT NULL,
    -- BLAKE3-keyed hash of the domain-separated normalized contact under the
    -- server-static pepper. The raw phone/email is NEVER stored (invariant
    -- #1) — only this 32-byte digest.
    contact_hash BINARY(32)       NOT NULL,
    -- NULL until /recovery-contact/enroll/finish proves the user controls the
    -- contact (a correct OTP). Only confirmed rows are usable for recovery.
    confirmed_at TIMESTAMP(3)     NULL,
    created_at   TIMESTAMP(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    -- One contact per (account, kind): a second email or phone for the same
    -- account replaces the prior one (the enroll path UPSERTs).
    PRIMARY KEY (account_id, kind),
    CONSTRAINT fk_recovery_contacts_account
        FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS otp_challenges (
    account_id BINARY(16)       NOT NULL,
    -- 1 = email, 2 = phone (matches recovery_contacts.kind).
    kind       TINYINT UNSIGNED NOT NULL,
    -- BLAKE3-keyed hash of the 6-digit code under the same server-static
    -- pepper. The plaintext code leaves the server only inside the outgoing
    -- message; the verify path recomputes this hash from the presented code
    -- and matches it.
    code_hash  BINARY(32)       NOT NULL,
    expires_at TIMESTAMP(3)     NOT NULL,
    -- Verification attempts against this challenge. >= 5 -> rejected, so the
    -- 6-digit code cannot be brute-forced inside its 10-minute window.
    attempts   TINYINT UNSIGNED NOT NULL DEFAULT 0,
    created_at TIMESTAMP(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    -- One live challenge per (account, kind): a new send UPSERTs over the
    -- prior code + resets `attempts`.
    PRIMARY KEY (account_id, kind),
    INDEX idx_otp_challenges_expires (expires_at),
    CONSTRAINT fk_otp_challenges_account
        FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
