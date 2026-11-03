-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Widen the sealed `/g/` column for the photo in a link preview (ADR-0087
-- section 18.4).
--
-- Every preview kind (a group, a co-publisher link, a channel subscriber
-- invitation) gains a trailing avatar field: a length byte and at most 170
-- bytes (a still and a video reference). The codec's bound on the sealed
-- blob grows 1824 -> 1995 (`lakko_contacts::group_invite::MAX_SEALED_LEN`).
-- `MAX_SEALED_BYTES` in `src/group_invite.rs` derives the same number from
-- the codec, and a test pins it against this file. The server still stores
-- ciphertext only.
--
-- Flag day: an in-place MODIFY, every other column unchanged.

ALTER TABLE group_invites
    MODIFY sealed_capability VARBINARY(1995) NOT NULL
        COMMENT 'lakko_contacts::link_seal sealed GroupInviteCapability; opaque without the link secret';
