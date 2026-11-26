-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0101 section 7: the channel recovery key.
--
-- The owner key is derived from the owning account's seed, so an ADR-0073
-- key reset loses it. Beside it the owner registers a second Ed25519 public
-- key, the channel recovery key, whose seed a device of the same ACCOUNT can
-- re-derive after a reset (an OPRF evaluation by `services/auth` under a
-- per-account key, the channel id blinded). `PUT /v1/channels/:id/recovery-key`
-- (owner proof) sets or replaces it; `PUT /v1/channels/:id/owner` accepts a
-- proof by it instead of the owner proof. A public key only: the service
-- learns no account (invariant #5).
--
-- Nullable: a channel registers one at its owner device's next pass. There
-- is no production deployment.

ALTER TABLE channels
    ADD COLUMN recovery_pk BINARY(32) NULL;
