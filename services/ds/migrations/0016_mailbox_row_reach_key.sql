-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0060 section 2, amended 2026-11-06 (maintainer decision): removing a
-- contact rotates the account's shared `contact` reach key, and the owner
-- keeps a few retired versions registered so a contact offline across a
-- rotation can still write. The owner's device must then tell a row written
-- under a retired key from one written under the current key, so each
-- mailbox row records the reach key that admitted it: the key a direct
-- reach proof named, or the issuer of a delegation. NULL for a proofless
-- write (an open or unclaimed mailbox). Returned to the mailbox owner only,
-- on its own signed read. Public keys only (invariant #2).
ALTER TABLE ds_mailbox_messages
    ADD COLUMN reach_pk BINARY(32) NULL;
