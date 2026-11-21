-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- The identity key's signature over its X25519 key (security review
-- 2026-11-21, `lakko_contacts::x25519_binding`).
--
-- Every sealed-to-account envelope is sealed to `x25519_pubkey`, which is
-- derived from a seed of its own and was tied to `signature_pubkey` by
-- nothing. `POST /discovery/identity` now requires a 64-byte Ed25519
-- signature by `signature_pubkey` over `x25519_pubkey`, verifies it, and
-- stores it here; `GET /discovery/identity/{id}` serves it beside the keys
-- so a client checks it before sealing. Replaced with the X25519 key on
-- every upload. A public artefact, like the keys (invariant #2).
--
-- Nullable: rows written before this migration, and rows the test fixtures
-- insert directly, carry none. There is no production deployment.

ALTER TABLE identity_bundles
    ADD COLUMN x25519_binding VARBINARY(64) NULL;
