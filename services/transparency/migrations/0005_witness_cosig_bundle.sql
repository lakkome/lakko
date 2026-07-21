-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0039 S3 (G-30 finding 1): K-of-N witness co-signature BUNDLE.
--
-- Sprint 3 makes the transparency service fan out to N witnesses and serve a
-- BUNDLE of co-signatures instead of one, so a single dishonest witness cannot
-- help the operator equivocate (a K-of-N quorum, enforced client-side).
--
-- The single-row `witnessed_root` (0003) stored ONE 105-byte co-signature in a
-- `cosig VARBINARY(160)` column. The bundle needs 0..N of them. Shape: keep the
-- single row and store the CONCATENATION of the fixed-105-byte
-- (`WITNESS_COSIG_LEN`) co-signatures in one widened column — the store chunks
-- it back on load (`chunk_cosigs`), which is unambiguous because every entry is
-- exactly 105 bytes.
--
-- Rename `cosig` -> `cosigs` (it now holds a bundle, not a single signature) and
-- widen to `VARBINARY(8192)` (room for ~78 co-signatures — far beyond any
-- realistic K-of-N deployment). A pre-S3 row carrying a single 105-byte
-- co-signature is preserved verbatim by the CHANGE and chunks back to a
-- one-element bundle on load, so an existing deployment upgrades transparently.
--
-- Still opaque to the Merkle tree (the co-signatures never enter `canonical_bytes`
-- — they bind only `(tree_root, log_size)`, invariant #2) and still a single row
-- (only the current head's bundle is cached; a restart restores it without
-- re-co-signing from scratch).
ALTER TABLE witnessed_root
    CHANGE COLUMN cosig cosigs VARBINARY(8192) NOT NULL
    COMMENT 'ADR-0039 S3: concatenated K-of-N witness cosig bundle (N x 105 bytes)';

-- ADR-0039 S3b (G-30 finding 1 residual): per-witness high-water — anti-stranding.
--
-- The S3 fan-out sent EVERY witness a consistency proof from ONE shared base (the
-- cached bundle's `log_size`) and advanced that shared base whenever ANY witness
-- succeeded. A witness that was DOWN during a partial-success growth round then
-- fell behind the shared base and could NEVER satisfy `verify_consistency` again
-- (its own high-water < the shared base, yet it was handed a proof FROM the shared
-- base) — permanently stranded until a manual reset. Over time witnesses drop out,
-- the quorum falls below K, and clients false-alarm.
--
-- The witness service (services/witness) keeps its OWN per-instance high-water and
-- verifies each proof against IT, so the fix is to give EACH witness a proof from
-- THAT witness's own last-co-signed position. We remember, per witness INDEX
-- (positional, matching the LAKKO_TRANSPARENCY_WITNESS_URLS order), the last
-- `(tree_root, log_size)` that witness successfully co-signed. On a growth round
-- the fan-out builds witness i's proof from `witness_high_water[i].log_size` (0 —
-- the genesis base — for a witness with no row, exactly the pre-S3 first-cosig
-- posture) to the current head, and advances that row ONLY on a successful cosign.
-- A witness that was down is left untouched, so the NEXT growth re-sends it a proof
-- from its still-old position and it auto-rejoins — no permanent stranding.
--
-- `log_size` is what the proof base needs; `tree_root` is retained for completeness
-- (the size it last co-signed at). Opaque to the Merkle tree (never in
-- `canonical_bytes`, invariant #2). One row per witness index.
CREATE TABLE IF NOT EXISTS witness_high_water (
    witness_index INT UNSIGNED    NOT NULL PRIMARY KEY,
    -- The `(tree_root, log_size)` this witness last successfully co-signed.
    tree_root     BINARY(32)      NOT NULL,
    log_size      BIGINT UNSIGNED NOT NULL,
    updated_at    TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3)
                                           ON UPDATE CURRENT_TIMESTAMP(3)
) ENGINE = InnoDB
  DEFAULT CHARSET = utf8mb4
  COLLATE = utf8mb4_unicode_ci;
