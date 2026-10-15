-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors
--
-- ADR-0078 section 1: a personal mailbox is named after a mailbox key, not the
-- identity key.
--
-- Until now `ds_mailbox_access` stored, per mailbox, the account's identity
-- key in the clear, so the mailbox table joined directly to discovery's
-- identity bundles. A mailbox is now addressed as
-- `BLAKE3-derive-key("lakko/mailbox/address/v1", mailbox_pk)` for a mailbox
-- key pair every device of the account derives from the account seed
-- (`lakko_contacts::mailbox`); the claim stores that `mailbox_pk`, and every
-- read and reach-key registration is signed by it. No column of this service
-- holds an identity key any more.
--
-- A flag day: nothing is deployed. The two tables are recreated rather than
-- altered (the primary key and the foreign key change name), and the rows of
-- the old mailboxes - stored before every row was sealed - are deleted, since
-- no client reads an identity-named mailbox any more.

DROP TABLE IF EXISTS ds_mailbox_reach;
DROP TABLE IF EXISTS ds_mailbox_access;

DELETE FROM ds_mailbox_messages;
DELETE FROM ds_delegation_nonces;

CREATE TABLE ds_mailbox_access (
    -- The mailbox address: `mailbox_address(mailbox_pk)`, checked at claim
    -- time, so a row can only ever name the key the address is the hash of.
    mailbox     BINARY(32)      NOT NULL,
    -- Ed25519 public key of the account's mailbox key (not its identity key).
    mailbox_pk  BINARY(32)      NOT NULL,
    -- True only in Everyone (ADR-0060). Named `is_open` because OPEN is a
    -- keyword in MariaDB's grammar.
    is_open     BOOLEAN         NOT NULL DEFAULT FALSE,
    created_at  TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (mailbox)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE ds_mailbox_reach (
    mailbox        BINARY(32)       NOT NULL,
    -- Ed25519 verifying key of one reach key.
    reach_pk       BINARY(32)       NOT NULL,
    -- Unix milliseconds after which the key stops being honoured; NULL for a
    -- key that lives until the owner removes it.
    expires_at_ms  BIGINT UNSIGNED  NULL,
    created_at     TIMESTAMP(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (mailbox, reach_pk),
    INDEX idx_reach_expires_at (expires_at_ms),
    CONSTRAINT fk_reach_mailbox
        FOREIGN KEY (mailbox) REFERENCES ds_mailbox_access(mailbox)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
