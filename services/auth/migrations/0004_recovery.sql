-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 1.1d — BIP39 mnemonic recovery.
--
-- The recovery flow lets a user re-establish access to an account after
-- losing every authenticated device. The user holds a 24-word BIP39
-- mnemonic (paper or metal backup); the client derives a 64-byte seed
-- from it (BIP39 PBKDF2-HMAC-SHA512 per the spec) and feeds the seed as
-- the "password" input to a parallel OPAQUE-PAKE registration.
--
-- The server stores up to TWO OPAQUE envelopes per account:
--
--   * kind = 1 (KIND_PASSWORD)  — derived from the user-typed password
--   * kind = 2 (KIND_RECOVERY)  — derived from the BIP39 mnemonic seed
--
-- Both envelopes are OPAQUE-blinded — the server cannot recover either
-- secret. To prevent cross-flow attacks (e.g. presenting a password
-- envelope when answering a recovery login challenge), the OPAQUE
-- credential_id is `account_id || kind_byte` — domain-separated per
-- kind. See `services/auth/src/opaque.rs` for the constants.
--
-- The accounts table is unchanged; recovery is keyed by account_id
-- with FK CASCADE on `accounts(id)`, identical to the password row.

-- ---------------------------------------------------------------------------
-- opaque_registrations: composite PK (account_id, kind)
-- ---------------------------------------------------------------------------

ALTER TABLE opaque_registrations
    DROP PRIMARY KEY,
    ADD COLUMN kind TINYINT UNSIGNED NOT NULL DEFAULT 1
        COMMENT '1 = password, 2 = recovery (BIP39 mnemonic seed)',
    ADD PRIMARY KEY (account_id, kind);

-- ---------------------------------------------------------------------------
-- opaque_login_states: kind tag so /login/finish vs /recovery/login/finish
-- can refuse a state created for the wrong flow (anti confused-deputy).
-- ---------------------------------------------------------------------------

ALTER TABLE opaque_login_states
    ADD COLUMN kind TINYINT UNSIGNED NOT NULL DEFAULT 1
        COMMENT '1 = password, 2 = recovery (BIP39 mnemonic seed)';
