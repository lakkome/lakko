-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0075, amended 2026-11-26 (second): the email vault is removed.
--
-- The vault sealed the phrase entropy under a key stretched from the login
-- handle, the address and a server-minted salt; the operator saw the address
-- and held the salt and the at-rest key, so it could open the vault. With
-- ADR-0101 (a reset gives an account its conversations back only when it is
-- sealed by a secret the server never holds) that vault would undo the seal.
-- A mailed code now gives a new password only, for every account, and serves
-- as a reset factor (ADR-0073, amended 2026-11-26).
--
-- Gone with it: the salt handed out at enrol start, the "phrase saved" mode
-- (the server keeps no flag; a device reads the account's seal
-- announcement), the "the phrase was ever in a vault" flag that decided
-- whether saving a phrase took a key rotation, the recall anchor's
-- `mail_protected` (the phrase arm of the recall now serves every account),
-- and the ticket's password mode (a verified code leads straight to the
-- password routes). The notices of kinds 2 ("account opened by email") and 6
-- ("recovery phrase saved") are retired, never reused.
--
-- Flag day: nothing is deployed for users.

DROP TABLE IF EXISTS email_vaults;

ALTER TABLE email_challenges DROP COLUMN vault_salt;

ALTER TABLE email_tickets DROP COLUMN password_mode;

ALTER TABLE accounts
    DROP COLUMN phrase_saved,
    DROP COLUMN phrase_in_vault;

ALTER TABLE recall_anchors DROP COLUMN mail_protected;

DELETE FROM account_notices WHERE kind IN (2, 6);
