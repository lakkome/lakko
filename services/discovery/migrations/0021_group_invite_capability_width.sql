-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Widen `group_invites.capability_bytes` to the longest v2 capability
-- (the 2026-09-28 security pass of ADR-0028).
--
-- The column was VARBINARY(512), sized for the v1 capability (at most 363
-- bytes). The v2 capability carries a sealed preview of up to
-- `MAX_SEALED_PREVIEW_LEN` = 543 bytes, so its maximum is 908 bytes:
-- schema version (1) + group_id_len (2) + group_id (256) + issuer key (32)
-- + expiry_ms (8) + preview_len (2) + sealed preview (543) + signature (64).
-- A capability whose preview named a long group or inviter was therefore
-- refused at upload and the client fell back to the long inline link.
-- `MAX_CAPABILITY_BYTES` in `src/group_invite.rs` derives the same number
-- from the codec's bounds, and a test pins it against this width.
--
-- Flag day: an in-place MODIFY, every other column unchanged.

ALTER TABLE group_invites
    MODIFY capability_bytes VARBINARY(908) NOT NULL
        COMMENT 'lakko_contacts::group_invite::GroupInviteCapability wire format (ADR-0028, v2 with the sealed preview)';
