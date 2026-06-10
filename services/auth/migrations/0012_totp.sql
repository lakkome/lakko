-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0018 (Recovery factor cascade, factor (c)) — TOTP (RFC 6238)
-- authenticator-code recovery.
--
-- One row per account holding the server-side TOTP verifier. Enrollment is
-- a two-step bearer-authed flow (`/totp/enroll/{start,finish}`): /start
-- mints a fresh 20-byte secret and stores it UNCONFIRMED; /finish proves
-- the user's authenticator actually has the secret (a correct current
-- code) before the factor becomes usable for recovery. `/totp/recover`
-- (anonymous + PoW) verifies a code against the CONFIRMED secret and
-- issues the standard session-token triple.
--
-- TRUST CLASS: the TOTP secret is an ACCESS verifier in the same trust
-- class as the OPAQUE password envelope — it authenticates the user, it
-- never decrypts E2EE data. Holding it does NOT violate invariant #2 (no
-- message / chain / root / media keys are involved); the classification is
-- recorded in ADR-0018 "Server-held verifiers are ACCESS verifiers".
--
-- AT-REST ENCRYPTION: unlike the grant tables (0009 / 0011) we cannot
-- store a hash — TOTP verification needs the raw secret to recompute the
-- HMAC. `secret_enc` is therefore the lakko-crypto sealed envelope
-- `nonce(12) || ChaCha20-Poly1305(secret)(20+16)` = 48 bytes under the
-- server-static `LAKKO_TOTP_ENC_KEY`. This defends a DB-ONLY leak (dump /
-- backup theft) — an attacker with the database but not the service
-- environment cannot mint codes. It does NOT defend a full-host
-- compromise (the key lives in the service env); that residual is
-- inherent to any server-verified OTP factor.
--
-- REPLAY / BRUTE-FORCE STATE: `last_accepted_step` makes an accepted code
-- single-use per 30-second step (the recover path requires the matched
-- step to be strictly greater, via an atomic compare-and-set UPDATE);
-- `failed_attempts` + `last_failed_at` drive the lockout (5 failures
-- within 15 minutes -> 429).

CREATE TABLE IF NOT EXISTS totp_secrets (
    account_id          BINARY(16)       NOT NULL PRIMARY KEY,
    -- lakko-crypto `seal_with_key` envelope of the 20-byte RFC 4226
    -- secret: nonce(12) || ciphertext+tag(36) = 48 bytes today; VARBINARY
    -- leaves headroom for a future longer secret.
    secret_enc          VARBINARY(64)    NOT NULL,
    -- NULL until /totp/enroll/finish proves the authenticator has the
    -- secret. Only confirmed rows are usable for recovery.
    confirmed_at        TIMESTAMP(3)     NULL,
    -- Highest RFC 6238 time step (floor(unix/30)) ever accepted for this
    -- account. A recover code is accepted only for a strictly greater
    -- step — an accepted code cannot be replayed within its window.
    last_accepted_step  BIGINT           NOT NULL DEFAULT 0,
    -- Consecutive failed recover attempts since the last success; reset
    -- to 0 on success. >= 5 within the 15-minute window -> 429.
    failed_attempts     TINYINT UNSIGNED NOT NULL DEFAULT 0,
    last_failed_at      TIMESTAMP(3)     NULL,
    created_at          TIMESTAMP(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    CONSTRAINT fk_totp_secrets_account
        FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
