-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0036 S4 (G-30 finding 2): auth-attested genesis sidecar.
--
-- Opaque to the Merkle tree (NEVER part of `canonical_bytes`, so it does
-- not affect any root or inclusion proof) — the additive-column precedent
-- set by `0002_leaf_auth.sql`. Carries the account-authenticated genesis
-- attestation (a 153-byte fixed-layout Ed25519 blob minted by
-- `services/auth` on `POST /auth/genesis/attest`, binding the
-- OPAQUE-authenticated account to the client-asserted genesis pubkey)
-- ALONGSIDE — not overloading — the `leaf_auth` self-signature column.
--
-- The store persists it verbatim next to `leaf_auth`; the read path
-- (`/transparency/latest`) serves it so a client can verify on a first
-- observation. It is never parsed except in the append gate, where — under
-- `LAKKO_TRANSPARENCY_REQUIRE_GENESIS_ATTEST` — a genesis/reset leaf must
-- carry one that `verify_genesis_fresh` accepts, else the append is refused
-- (fail-closed; the operator-side defence-in-depth reject that turns
-- first-entry squatting from DETECTED into PREVENTED).
--
-- Nullable: additive + non-breaking for existing genesis leaves (which
-- carry none), and the append handler's presence policy governs whether an
-- absent value is refused (default OFF — a deploy rollout, sequenced after
-- the three clients mint). `VARBINARY(256)` = the 153-byte attestation with
-- ample headroom.
ALTER TABLE log_entries
    ADD COLUMN genesis_attest VARBINARY(256) NULL
    COMMENT 'ADR-0036: auth-attested genesis (153B GenesisAttestation); opaque sidecar';
