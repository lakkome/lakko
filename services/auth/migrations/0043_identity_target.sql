-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0099 step 5: a moderator statement names an account by its target,
-- `BLAKE3-derive-key("lakko/moderation/account-target/v1", identity_pubkey)`
-- (`lakko_moderation::target::account_target_hash`), and the moderation
-- service no longer hands auth the identity key beside it.
--
-- 1. `accounts.identity_target`: the target of `identity_pubkey`, written in
--    the same statement as the key by every writer (the genesis mint, the
--    key reset, the key rotation, the recall). Suspend and Lift find their
--    accounts by it. The index on `identity_pubkey` that served the old
--    lookup goes.
--
-- 2. `account_suspensions.target`: the target the suspending statement
--    named, so the panel's appeal list and a decline name the suspension
--    by it even after the account's key changed.

ALTER TABLE accounts
    ADD COLUMN identity_target BINARY(32) NULL
        COMMENT 'account_target_hash(identity_pubkey); NULL while no key is recorded';

CREATE INDEX idx_accounts_identity_target ON accounts (identity_target);

DROP INDEX idx_accounts_identity_pubkey ON accounts;

ALTER TABLE account_suspensions
    ADD COLUMN target BINARY(32) NULL
        COMMENT 'the account target the suspending statement named';

CREATE INDEX idx_account_suspensions_target ON account_suspensions (target);
