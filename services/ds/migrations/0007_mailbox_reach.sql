-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0058 section 2: an identity mailbox accepts a write only when it is
-- signed with a reach key the owner registered.
--
-- A claimed mailbox (a `ds_mailbox_access` row) is now CLOSED unless its
-- owner opens it (the Everyone setting). A closed mailbox stores a write only
-- when it carries an `x-lakko-ds-proof` at the reach epoch that verifies
-- under one of the reach keys below, or a one-write delegation signed by one
-- of them. An unclaimed mailbox (the rendezvous mailboxes) is unchanged.
--
-- The server stores PUBLIC keys only. A reach key is shared per class and
-- scope, never one per contact, so the row count tells the server roughly how
-- many groups, channels and live links a person has - not how many contacts
-- (the ADR's Residuals section).

-- `is_open`: true only in Everyone. Named `is_open` rather than `open`
-- because OPEN is a keyword in MariaDB's grammar.
ALTER TABLE ds_mailbox_access
    ADD COLUMN is_open BOOLEAN NOT NULL DEFAULT FALSE;

CREATE TABLE IF NOT EXISTS ds_mailbox_reach (
    mailbox        BINARY(32)       NOT NULL,
    -- Ed25519 verifying key of one reach key.
    reach_pk       BINARY(32)       NOT NULL,
    -- Unix milliseconds after which the key stops being honoured; NULL for
    -- a key that lives until the owner removes it. Unsigned so any u64 the
    -- owner sends fits; the pruner deletes rows past it.
    expires_at_ms  BIGINT UNSIGNED  NULL,
    created_at     TIMESTAMP(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (mailbox, reach_pk),
    INDEX idx_reach_expires_at (expires_at_ms),
    CONSTRAINT fk_reach_mailbox
        FOREIGN KEY (mailbox) REFERENCES ds_mailbox_access(device_id)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Spent delegation nonces. A delegation is good for one write, and lives up
-- to 24 hours (lakko_ds_proof::MAX_DELEGATION_TTL_MS). Not kept in
-- `ds_proof_nonces`: that table is pruned two proof windows (four minutes)
-- after a nonce is seen, so a delegation replayed an hour later would find
-- its nonce gone. A row here lives until the delegation it names has
-- expired, after which the delegation is refused on its expiry alone.
CREATE TABLE IF NOT EXISTS ds_delegation_nonces (
    mailbox        BINARY(32)       NOT NULL,
    nonce          BINARY(16)       NOT NULL,
    expires_at_ms  BIGINT UNSIGNED  NOT NULL,
    PRIMARY KEY (mailbox, nonce),
    INDEX idx_delegation_nonce_expires_at (expires_at_ms)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
