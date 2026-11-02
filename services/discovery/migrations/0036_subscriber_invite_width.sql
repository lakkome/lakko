-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Widen the sealed `/g/` column for ADR-0087 sections 5 and 7.
--
-- A group capability now names the group's knock key (a 32-byte key and a
-- 2-byte version after its flag byte), and a `/g/` link may be a channel
-- subscriber invitation (preview kind 3: the channel's invite seed or read
-- key, its admission key and that key's version, and a description of up to
-- 1024 bytes). The codec's bound on the sealed blob grows 938 -> 1824
-- (`lakko_contacts::group_invite::MAX_SEALED_LEN`). `MAX_SEALED_BYTES` in
-- `src/group_invite.rs` derives the same number from the codec, and a test
-- pins it against this file. The server still stores ciphertext only.
--
-- Flag day: an in-place MODIFY, every other column unchanged.

ALTER TABLE group_invites
    MODIFY sealed_capability VARBINARY(1824) NOT NULL
        COMMENT 'lakko_contacts::link_seal sealed GroupInviteCapability; opaque without the link secret';
