-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0060 section 3: a member card is bound to the account behind it.
--
-- The sealed card plaintext now opens with the account identity key (32
-- bytes) and its Ed25519 signature (64 bytes) before the profile, so the
-- longest card (`lakko_channels::MEMBER_CARD_MAX_LEN`) grew by 96 bytes:
-- 1057 -> 1153. `api.rs` checks the length at handler entry against the
-- codec's constant, and a test pins this file to it.
--
-- Flag day: an in-place MODIFY; cards of the previous version are refused by
-- every reader anyway (version byte 1 -> 2).

ALTER TABLE channel_member_cards
    MODIFY COLUMN card VARBINARY(1153) NOT NULL;
