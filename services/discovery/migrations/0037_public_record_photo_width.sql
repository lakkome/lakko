-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Widen `public_records.record` for the photo of a public group or channel
-- (ADR-0087 section 18.3).
--
-- The handle record payload gains an avatar field after the size band (a
-- length byte and at most 170 bytes: a still and a video reference), and its
-- first byte moves from 0x41 to 0x42. The codec's bound on a public record
-- grows 7411 -> 7582 (`lakko_contacts::introduction::PUBLIC_RECORD_MAX_LEN`).
-- `MAX_PUBLIC_RECORD_LEN` in `src/visibility.rs` derives the same number
-- from the codec, and a test pins it against this file. The server still
-- stores ciphertext only.
--
-- Flag day: an in-place MODIFY, every other column unchanged.

ALTER TABLE public_records
    MODIFY record VARBINARY(7582) NOT NULL;
