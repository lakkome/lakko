-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- G-13 (ADR-0014) — server-side enforcement of WebAuthn action binding.
--
-- The client-side action ceremony (`/auth/action/{start,finish}`) forces a
-- fresh passkey gesture, but a stolen session token replayed directly
-- against a sensitive endpoint would bypass a purely client-side gate. To
-- make the gate real, `/auth/action/finish` writes a short-lived,
-- single-use *grant* on success, and the sensitive endpoint (currently
-- `DELETE /webauthn/credentials/:id`) recomputes the expected `action_hash`
-- from its own parameters and atomically consumes a matching grant before
-- performing the operation. No grant -> the operation is refused.
--
-- `action_hash` is the same 32-byte value the client bound the assertion
-- to: `SHA-256("lakko/action/<op>/v1" || identifier)`. The PRIMARY KEY
-- (account_id, action_hash) keeps one live grant per (account, action); a
-- re-issued ceremony refreshes `expires_at`. Consumption is a row DELETE,
-- so a grant is strictly single-use. The pruner sweeps expired rows.

CREATE TABLE IF NOT EXISTS webauthn_action_grants (
    account_id   BINARY(16)  NOT NULL,
    -- SHA-256 over the action's domain-separated preimage. Opaque to this
    -- table; the consuming endpoint recomputes and matches it.
    action_hash  BINARY(32)  NOT NULL,
    expires_at   TIMESTAMP   NOT NULL,
    PRIMARY KEY (account_id, action_hash),
    INDEX idx_webauthn_action_grants_expires (expires_at),
    CONSTRAINT fk_webauthn_action_grants_account
        FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
