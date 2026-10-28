-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Widen the sealed `/i/` column for ADR-0079 section 1.
--
-- The contact-invite capability now carries an inviter preview (the owner's
-- display name, at most 255 bytes, and username, at most 32 bytes, each with
-- a one-byte length), sealed under the link secret with the rest of the
-- capability. The longest sealed blob grows by 289 bytes, 404 -> 693
-- (`lakko_contacts::invite::MAX_SEALED_LEN`). `MAX_SEALED_BYTES` in
-- `src/invite.rs` derives the same number from the codec, and a test pins it
-- against this file. The server still stores ciphertext only.
--
-- Flag day: an in-place MODIFY, every other column unchanged.

ALTER TABLE invites
    MODIFY sealed_capability VARBINARY(693) NOT NULL
        COMMENT 'lakko_contacts::link_seal sealed InviteCapability; opaque without the link secret';
