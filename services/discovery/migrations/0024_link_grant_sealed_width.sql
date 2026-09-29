-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Widen the sealed `/i/` and `/c/` columns to the v2 capabilities of
-- ADR-0058 section 6.
--
-- Both capabilities now carry, inside their signed and sealed part, a
-- 16-byte `link_id` and the link's 37-byte `link` reach grant, so the longest
-- capability wire of each grew by 53 bytes and so did the longest sealed blob
-- (`lakko_contacts::{invite,call_invite}::MAX_SEALED_LEN`): 254 -> 307 and
-- 4338 -> 4391. `MAX_SEALED_BYTES` in `src/invite.rs` and
-- `src/call_invite.rs` derives the same numbers from the codecs, and a test in
-- each pins it against this file.
--
-- `/g/` is unchanged: its capability carries no grant (the joiner's reply
-- grant rides in the join attribution instead).
--
-- Flag day: an in-place MODIFY, every other column unchanged.

ALTER TABLE invites
    MODIFY sealed_capability VARBINARY(307) NOT NULL
        COMMENT 'lakko_contacts::link_seal sealed InviteCapability; opaque without the link secret';

ALTER TABLE call_invites
    MODIFY sealed_capability VARBINARY(4391) NOT NULL
        COMMENT 'lakko_contacts::link_seal sealed CallInviteCapability; opaque without the link secret';
