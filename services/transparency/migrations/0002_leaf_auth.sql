-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- G-30 #2 (ADR-0019 Sprint 2): client leaf authorisation sidecar.
--
-- Opaque to the Merkle tree (NEVER part of `canonical_bytes`, so it does
-- not affect any root or inclusion proof). Stored so the operator can
-- verify the ACCOUNT authorised the `(account_key -> pubkey)` binding —
-- a genesis / fresh-start self-signature (64 bytes, signed by the leaf's
-- own key) or a normal rotation's supersession proof (145 bytes, signed
-- by the previous key) — and REJECT a fabricated (keyless) leaf. The
-- client-side rotation chain remains the real gate; this is the
-- defence-in-depth, log-hygiene reject (the server is untrusted).
--
-- Nullable: the column is back-fillable and the append handler's
-- presence policy (`LAKKO_TRANSPARENCY_REQUIRE_LEAF_AUTH`) governs
-- whether an absent value is refused.
ALTER TABLE log_entries
    ADD COLUMN leaf_auth VARBINARY(200) NULL
    COMMENT 'G-30 #2: genesis self-sig (64B) or supersession proof (145B); opaque sidecar';
