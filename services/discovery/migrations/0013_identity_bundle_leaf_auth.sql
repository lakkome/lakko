-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- G-30 #2 (ADR-0019) — client leaf authorisation, threaded through discovery.
--
-- Adds an optional, server-opaque `leaf_auth` to the identity bundle. On a
-- key change the client uploads a leaf authorisation alongside its new
-- bundle: a genesis / fresh-start SELF-signature (64 bytes, signed by the
-- leaf's own key over the `(account_key, pubkey)` binding) for a first entry
-- or an `is_reset` mint, OR a normal-rotation supersession proof (145 bytes,
-- signed by the previous key). Discovery stores it so it can forward it to
-- `services/transparency` on the best-effort append AND so the S2.5
-- reconciler can backfill it on a re-append.
--
-- The column is NULL by default. A non-rotation re-upload (no value)
-- PRESERVES any stored leaf_auth (the upsert uses COALESCE), exactly like
-- `supersession_proof`; a new rotation supplies a fresh value that replaces
-- it. Discovery NEVER parses these bytes -- it stores and forwards them
-- verbatim (invariant #2); the leaf authorisation is verified operator-side
-- in `services/transparency` and, definitively, by the client rotation
-- chain. The value is a public, key-signed artifact, so storing it leaks no
-- secret. Width is the 145-byte supersession proof + headroom, matching the
-- transparency-side `log_entries.leaf_auth` column.
ALTER TABLE identity_bundles
    ADD COLUMN leaf_auth VARBINARY(200) NULL
        COMMENT 'G-30 #2 (ADR-0019): optional client leaf authorisation (genesis self-sig 64B or supersession proof 145B), opaque to discovery, preserved across non-rotation re-uploads, replaced on a new rotation, forwarded to transparency append';
