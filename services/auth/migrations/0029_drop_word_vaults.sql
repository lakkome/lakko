-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- The three-word check moved to the device (2026-10-02, superseding
-- ADR-0072 decision 5 as first written).
--
-- A device that holds an account's keys also holds the recovery phrase under
-- its device key. It asks for three words at random positions and compares
-- them itself (`lakko_mnemonic::word_check`), then signs in and replaces the
-- password with the phrase seed it already has, through the existing
-- `/recovery/login/*` and `/password/reset/*` routes. The server takes no
-- part: nothing about the three words is stored here or sent here.
--
-- So the server-side verifier goes, with everything that served it:
--
--   word_vaults   positions, salt, Argon2id cost, the wrapped verifier, the
--                 miss counter and the tombstone marker (0017, 0023, 0027)
--   vault_opens   the per-account open log 0017 created; nothing reads or
--                 writes it any more
--
-- and the routes `GET /vault`, `POST /vault/enroll`, `DELETE /vault`,
-- `POST /vault/recover/start` and `POST /vault/recover/finish`.
--
-- What this makes moot in earlier migrations, which are left as written (a
-- migration file is checksummed):
--
--   0027  its header's description of `word_vaults` (verifier, trust class,
--         tombstone) and its `ALTER TABLE word_vaults` no longer describe
--         anything that exists. Its `passkey_vaults` part stands: the column
--         comment and the flag-day delete of blobs sealed for a 24-word
--         phrase.
--   0023  item 3 (the word vault's tombstone) is moot; its other items stand.
--
-- `passkey_vaults` is untouched: one opaque blob per passkey, the 16-byte
-- phrase entropy sealed by the client under a key derived from that
-- passkey's WebAuthn PRF output, wrapped again under LAKKO_VAULT_ENC_KEY.
--
-- `auth_name_limits` rows with scope 5 (the per-handle guess counter of
-- `/vault/recover/finish`) are removed; the scope number is not reused.

DROP TABLE IF EXISTS vault_opens;
DROP TABLE IF EXISTS word_vaults;

DELETE FROM auth_name_limits WHERE scope = 5;
