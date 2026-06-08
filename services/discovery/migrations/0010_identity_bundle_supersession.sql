-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- G-4 (ADR-0016) — identity-supersession proof transport, Carrier B.
--
-- Adds an optional, server-opaque supersession proof to the identity
-- bundle. When a user recovers their account (recover-from-words -> new
-- AccountIdentity), the client uploads its new bundle carrying a proof
-- signed by the OLD account key authorising the new key. A peer that has
-- the old key pinned fetches the bundle, verifies the proof locally, and
-- re-pins -- bridging trust without a blind TOFU (security invariant #6).
--
-- The column is NULL by default and is cleared on any non-rotation
-- re-upload (the upsert binds VALUES(supersession_proof), so a fresh
-- upload without a proof writes NULL). The server NEVER parses these
-- bytes -- it stores and serves them verbatim (invariant #2). The proof
-- is a public, old-key-signed artifact (crates/lakko-transparency), so
-- storing it leaks no secret. Width is the 145-byte proof + headroom.
ALTER TABLE identity_bundles
    ADD COLUMN supersession_proof VARBINARY(150) NULL
        COMMENT 'G-4 (ADR-0016): optional old-key-signed supersession proof, opaque to the server, cleared on non-rotation re-upload';
