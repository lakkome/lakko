-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- G-13 (ADR-0014) — WebAuthn action binding.
--
-- The action-bound auth flow reuses the short-lived `webauthn_states`
-- table (kind = 3 = action-bound `AuthenticationState`) but must record
-- WHICH action the issued challenge is bound to. The challenge itself is
--   BLAKE3_keyed("lakko/webauthn-action/v1", action_hash || server_nonce)
-- and is carried inside the serialized `AuthenticationState`, so the
-- cryptographic binding does not depend on this column. The column exists
-- only for the `/auth/action/finish` defence-in-depth check: the caller
-- re-presents the `action_hash`, and the server refuses if it does not
-- match the one the challenge was minted for (the signature already
-- guarantees this, the compare is belt-and-braces).
--
-- NULL for the existing kinds (0 = PasskeyRegistration, 1 =
-- PasskeyAuthentication, 2 = DiscoverableAuthentication); only kind = 3
-- populates it.

ALTER TABLE webauthn_states
    ADD COLUMN action_hash BINARY(32) NULL AFTER kind;
