-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0101 S2: the continuity binding of an account that reset its keys.
--
-- After an ADR-0073 key reset the resetting device announces a second
-- mailbox, the continuity mailbox, for a window: a 105-byte binding
-- (`lakko_contacts::continuity`) signed by the account's new identity key
-- over the continuity mailbox key and the end of the window. It is uploaded
-- with `PUT /discovery/identity/continuity`, verified there against the
-- identity key this row holds, and served by `GET /discovery/identity/{id}`
-- while unexpired. A public artefact signed by a public key (invariant #2).
--
-- `continuity_expires_at_ms` repeats the binding's own expiry so the fetch
-- and the hourly sweep that nulls both columns never parse the binding; the
-- index keeps that sweep from scanning every row. A key change on upload
-- clears both columns: a binding names the key that signed it.
--
-- Nullable: almost every account never resets. There is no production
-- deployment.

ALTER TABLE identity_bundles
    ADD COLUMN continuity_binding VARBINARY(105) NULL,
    ADD COLUMN continuity_expires_at_ms BIGINT UNSIGNED NULL,
    ADD INDEX idx_identity_bundles_continuity_expiry (continuity_expires_at_ms);
