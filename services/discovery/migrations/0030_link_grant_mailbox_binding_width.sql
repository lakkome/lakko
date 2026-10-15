-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Widen the sealed `/i/` and `/c/` columns for ADR-0078 section 1.
--
-- The `link` reach grant inside both capabilities now carries the owner's
-- 97-byte mailbox binding (the mailbox key signed by the account identity
-- key), so the grant grew from 37 to 134 bytes and the longest sealed blob of
-- each kind by 97 (`lakko_contacts::{invite,call_invite}::MAX_SEALED_LEN`):
-- 307 -> 404 and 4391 -> 4488. `MAX_SEALED_BYTES` in `src/invite.rs` and
-- `src/call_invite.rs` derives the same numbers from the codecs, and a test in
-- each pins it against this file.
--
-- `/g/` is unchanged: its capability carries no grant.
--
-- Flag day: an in-place MODIFY, every other column unchanged.

ALTER TABLE invites
    MODIFY sealed_capability VARBINARY(404) NOT NULL
        COMMENT 'lakko_contacts::link_seal sealed InviteCapability; opaque without the link secret';

ALTER TABLE call_invites
    MODIFY sealed_capability VARBINARY(4488) NOT NULL
        COMMENT 'lakko_contacts::link_seal sealed CallInviteCapability; opaque without the link secret';
