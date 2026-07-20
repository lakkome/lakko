-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0036 S5 (G-30 finding 2): auth-attested genesis, threaded through
-- discovery.
--
-- Adds an optional, server-opaque `genesis_attest` to the identity bundle,
-- ALONGSIDE — not overloading — the `leaf_auth` self-signature column. On a
-- genesis / fresh-start (`is_reset`) key change the client uploads a
-- `GenesisAttestation` (a 153-byte fixed-layout Ed25519 blob minted by
-- `services/auth` on `POST /auth/genesis/attest`, binding the
-- OPAQUE-authenticated account to the client-asserted genesis pubkey)
-- alongside its new bundle. Discovery stores it so it can forward it to
-- `services/transparency` on the best-effort append (as `genesis_attest_b64`)
-- AND so the S2.5 reconciler can backfill it on a re-append.
--
-- The column is NULL by default. A non-rotation re-upload (no value)
-- PRESERVES any stored genesis_attest (the upsert uses COALESCE), exactly
-- like `leaf_auth` and `supersession_proof`; a new genesis / reset supplies a
-- fresh value that replaces it. A rotation carries no genesis attestation
-- (its `leaf_auth` supersession proof already authorises the binding), so the
-- COALESCE preserves the genesis leaf's value — harmless, since the
-- transparency append gate consults `genesis_attest` only on the
-- genesis/reset arm. Discovery NEVER parses these bytes -- it stores and
-- forwards them verbatim (invariant #2); the attestation is verified
-- operator-side in `services/transparency` (under
-- `LAKKO_TRANSPARENCY_REQUIRE_GENESIS_ATTEST`) and, definitively, by the
-- client on a first observation. The value is a public, key-signed artifact,
-- so storing it leaks no secret. `VARBINARY(256)` = the 153-byte attestation
-- with headroom, matching the transparency-side `log_entries.genesis_attest`
-- column.
ALTER TABLE identity_bundles
    ADD COLUMN genesis_attest VARBINARY(256) NULL
        COMMENT 'ADR-0036: optional auth-attested genesis attestation (153B GenesisAttestation), opaque to discovery, preserved across non-rotation re-uploads, replaced on a new genesis/reset, forwarded to transparency append as genesis_attest_b64';
