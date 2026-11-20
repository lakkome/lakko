-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors
--
-- The two backup-head tables (migrations 0015, 0016) had no foreign key to
-- `accounts`, so a deleted account left its row in each for good (found by
-- the 2026-11-19 data inventory for the privacy policy). Rows of accounts
-- that no longer exist are removed, then both tables cascade with the
-- account, on DELETE /account and on the unclaimed-account sweep alike.

DELETE FROM backup_head_versions
 WHERE account_id NOT IN (SELECT id FROM accounts);
DELETE FROM backup_head_mint_limits
 WHERE account_id NOT IN (SELECT id FROM accounts);

ALTER TABLE backup_head_versions
    ADD CONSTRAINT fk_backup_head_versions_account
    FOREIGN KEY (account_id) REFERENCES accounts (id) ON DELETE CASCADE;
ALTER TABLE backup_head_mint_limits
    ADD CONSTRAINT fk_backup_head_mint_limits_account
    FOREIGN KEY (account_id) REFERENCES accounts (id) ON DELETE CASCADE;
