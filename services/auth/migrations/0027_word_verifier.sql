-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0064 decision 5: three words sign the person in, and nothing else.
--
-- This header replaces the description in 0017's header, which is left as
-- written (a migration file is checksummed).
--
-- `word_vaults` no longer holds a ciphertext of anything the account owns.
-- One row per account that ever enrolled the three-word sign-in:
--
--   positions        three distinct ascending 1-based word positions in 1..=12
--   salt, argon2_*   what the client needs to stretch its answer; public
--   verifier_enc     seal_with_key(LAKKO_VAULT_ENC_KEY, verifier(32)), where
--                    verifier = SHA-256(HKDF(Argon2id(three words, salt),
--                    "lakko/vault/verify/v2")). NULL = the sign-in is off
--                    (a tombstone, see below)
--   failed_attempts  consecutive wrong answers since the last right one
--   off_reason       why verifier_enc is NULL: 1 = the account turned the
--                    sign-in off, 2 = ten wrong answers in a row; NULL
--                    while the sign-in is on
--
-- TRUST CLASS. The verifier is an ACCESS verifier, the class ADR-0018 names
-- for the TOTP secret: a correct answer is exchanged for a session token and
-- for nothing else. The invariant-#2 exception ADR-0049 section 6 proposed
-- for this table is WITHDRAWN: there is no sealed entropy here any more. A
-- party holding this table and LAKKO_VAULT_ENC_KEY can still search the 2^33
-- possible answers offline at one Argon2id evaluation each and learn the
-- three words (33 bits of the 132-bit phrase string); it learns no key
-- material.
--
-- TOMBSTONE. A row is never deleted while its account exists. Turning the
-- sign-in off (by the account, or by the tenth consecutive wrong answer)
-- sets verifier_enc to NULL and keeps positions, salt and cost, so
-- `/vault/recover/start` answers byte for byte as it did before and every
-- later answer misses. Deleting the row would make start switch to decoys,
-- which would tell a caller that a row had existed (migration 0023).
--
-- `passkey_vaults` is unchanged in shape: one opaque blob per passkey, the
-- 16-byte phrase entropy sealed by the client under a key derived from that
-- passkey's WebAuthn PRF output, wrapped again under LAKKO_VAULT_ENC_KEY.
-- The server cannot open or search it.
--
-- FLAG DAY (no deployment, no users): every existing row holds a value
-- computed for a 24-word phrase and can never match or open again.

DELETE FROM word_vaults;
DELETE FROM passkey_vaults;

ALTER TABLE word_vaults
    CHANGE body_enc verifier_enc VARBINARY(96) NULL
        COMMENT 'seal_with_key(vault_key, verifier(32)); NULL = off (tombstone)',
    ADD COLUMN off_reason TINYINT UNSIGNED NULL
        COMMENT '1 = turned off by the account, 2 = too many wrong tries';

ALTER TABLE passkey_vaults
    MODIFY body_enc VARBINARY(255) NOT NULL
        COMMENT 'seal_with_key(vault_key, client blob: the 16-byte entropy sealed under the passkey PRF key)';
