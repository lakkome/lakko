-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors
--
-- ADR-0074 section 3.7: the two-way conversation link of an account that
-- rotated, signed by the key its conversations are bound to and by the key
-- published here. Opaque to discovery (invariant #2): never parsed, kept across
-- a re-upload of the same key, replaced by a key change. The peer verifies it
-- against the key it pinned. `VARBINARY(256)` is the headroom of the sibling
-- attestation columns; the link is 201 bytes.
ALTER TABLE identity_bundles
    ADD COLUMN conversation_link VARBINARY(256) NULL
    COMMENT 'ADR-0074 3.7: opaque two-way conversation link';
