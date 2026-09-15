-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0049 (Identity recovery) — the word vault.
--
-- One row per account holding the recovery-phrase ENTROPY sealed by the
-- client under a key Argon2id-stretched from three of the user's 24 words
-- (the words at `positions`), plus a verifier the server compares a guess
-- against. `POST /vault/enroll` (bearer) writes it; `POST
-- /vault/recover/start` (anonymous + PoW) serves `positions`, `salt` and
-- the cost; `POST /vault/recover/finish` (anonymous + PoW) compares the
-- guess and, on a match, returns the sealed entropy the client opens.
--
-- TRUST CLASS — THIS IS THE STATED INVARIANT-#2 EXCEPTION. The server holds
-- an artefact from which the identity seed CAN be recovered at a cost: an
-- operator ignoring the guess counter below and holding LAKKO_VAULT_ENC_KEY
-- faces a 2^33 memory-hard search per account (three BIP39 words). The
-- server can never open it in one step. ADR-0049 section 6 records the
-- exception and the proposed wording for invariant #2.
--
-- AT-REST ENCRYPTION: `body_enc` is the lakko-crypto `seal_with_key`
-- envelope of `verifier(32) || sealed_entropy` under the server-static
-- LAKKO_VAULT_ENC_KEY (the 0012 TOTP pattern), so a database dump without
-- the service environment yields nothing to search.
--
-- GUESS BUDGET: `failed_attempts` + `last_failed_at` drive the 5-in-15-
-- minutes lockout (429); the 10th consecutive failure DELETES the row (the
-- three-word route then answers like an unknown account and the user falls
-- back to the 24 words or another device). A success resets the count.
--
-- `positions`, `salt` and the Argon2 cost are public by construction; an
-- unknown account is answered with pepper-derived decoys so /recover/start
-- is enumeration-safe.

CREATE TABLE IF NOT EXISTS word_vaults (
    account_id       BINARY(16)       NOT NULL PRIMARY KEY,
    -- Three distinct 1-based word positions, ascending.
    positions        BINARY(3)        NOT NULL,
    salt             BINARY(16)       NOT NULL,
    -- Argon2id cost the client enrolled with: memory (KiB), passes, lanes.
    -- Carried per row so the budget can be raised without breaking what
    -- was sealed under the old values.
    argon2_m_kib     INT UNSIGNED     NOT NULL,
    argon2_t         INT UNSIGNED     NOT NULL,
    argon2_p         INT UNSIGNED     NOT NULL,
    -- seal_with_key(vault_key, verifier(32) || sealed_entropy(60)).
    body_enc         VARBINARY(255)   NOT NULL,
    -- Consecutive failed recover attempts since the last success.
    failed_attempts  TINYINT UNSIGNED NOT NULL DEFAULT 0,
    last_failed_at   TIMESTAMP(3)     NULL,
    created_at       TIMESTAMP(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    CONSTRAINT fk_word_vaults_account
        FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Every successful open, so the account's other devices can later be told
-- about a recent recovery (the consumer is not built yet).
CREATE TABLE IF NOT EXISTS vault_opens (
    account_id  BINARY(16)   NOT NULL,
    opened_at   TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    INDEX idx_vault_opens_account (account_id, opened_at),
    CONSTRAINT fk_vault_opens_account
        FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
