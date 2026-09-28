-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0058: the rendezvous namespace, apart from personal mailboxes.
--
-- Pairing, recovery, the welcome courier and the state bundle meet at random
-- 32-byte rendezvous ids that both sides know and that are read without a
-- proof. They used to be unclaimed rows of `ds_mailbox_messages`, which is
-- why an unclaimed mailbox had to stay readable - and that in turn let anyone
-- holding an identity key tell whether its mailbox was claimed (200 for an
-- unclaimed one, 401 for a claimed one). They now live here, behind their own
-- routes (`/ds/rendezvous/{hex}/messages`), and `/ds/mailbox/` is personal
-- only: a read without the identity proof is the uniform 401 whatever the
-- mailbox's state.
--
-- A separate table rather than a key prefix in `ds_mailbox_messages`: a
-- rendezvous id and a mailbox pseudonym are both arbitrary 32-byte values, so
-- only a separate key space makes a collision impossible by construction - a
-- rendezvous id equal to someone's mailbox pseudonym names a different row
-- set. No claim, no reach keys, no proof. Same shape, the same 30-day
-- retention as the mailbox journal (the pruner walks both).

CREATE TABLE IF NOT EXISTS ds_rendezvous_messages (
    id             BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    rendezvous_id  BINARY(32)      NOT NULL,
    payload        LONGBLOB        NOT NULL,
    created_at     TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (id),
    INDEX idx_rendezvous_id_id (rendezvous_id, id),
    INDEX idx_created_at (created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
