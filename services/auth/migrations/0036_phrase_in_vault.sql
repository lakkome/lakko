-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors
--
-- ADR-0075 section 3, amended 2026-11-01: saving a recovery phrase changes the
-- keys only when the account's current phrase was ever sealed in an email
-- vault, since only then can a copy taken earlier open it. Set on a vault
-- write, cleared only by what changes the phrase (rotation, reset, recall), so
-- removing the email does not make such a phrase safe to show.
ALTER TABLE accounts
    ADD COLUMN phrase_in_vault TINYINT(1) NOT NULL DEFAULT 0
    COMMENT 'the current phrase was ever sealed in an email vault';

UPDATE accounts SET phrase_in_vault = 1
WHERE id IN (SELECT account_id FROM email_vaults);
