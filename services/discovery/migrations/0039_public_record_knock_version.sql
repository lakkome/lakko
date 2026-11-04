-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Widen `public_records.record` for the knock version of a public group
-- (ADR-0087 section 4).
--
-- The handle record payload gains the knock key's version (a big-endian
-- u16) right after the knock key, so a person who found a public group
-- through search seals its knock row to the version the admins open; its
-- first byte moves from 0x42 to 0x43. The codec's bound on a public record
-- grows 7582 -> 7584 (`lakko_contacts::introduction::PUBLIC_RECORD_MAX_LEN`).
-- `MAX_PUBLIC_RECORD_LEN` in `src/visibility.rs` derives the same number
-- from the codec, and a test pins it against this file. The server still
-- stores ciphertext only.
--
-- Flag day: an in-place MODIFY, every other column unchanged.

ALTER TABLE public_records
    MODIFY record VARBINARY(7584) NOT NULL;
