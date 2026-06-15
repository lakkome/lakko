-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0020 Sprint 4 — the last operator root the WITNESS has co-signed.
--
-- The witness (services/witness) co-signs an operator root only after a Merkle
-- consistency proof shows it append-only-extends the witness's last-co-signed
-- root. The transparency service caches the resulting co-signature here so
-- `GET /transparency/root` can serve it (clients require operator + witness
-- signatures, the split-view defence) and so a restart restores the same
-- witnessed `(tree_root, log_size)` without re-co-signing from scratch.
--
-- Single-row table. Only the `(tree_root, log_size)` bind the co-signature
-- (the served root is re-signed fresh on every request — the operator
-- signature carries a timestamp, the co-signature does not), so those plus the
-- opaque co-signature blob are all we persist.
CREATE TABLE IF NOT EXISTS witnessed_root (
    id        TINYINT UNSIGNED NOT NULL PRIMARY KEY CHECK (id = 1),
    -- The witnessed Merkle root (32 bytes) + its size in leaves.
    tree_root BINARY(32)       NOT NULL,
    log_size  BIGINT UNSIGNED  NOT NULL,
    -- The witness co-signature wire blob (105 bytes; VARBINARY leaves room).
    cosig     VARBINARY(160)   NOT NULL,
    updated_at TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3)
                                          ON UPDATE CURRENT_TIMESTAMP(3)
) ENGINE = InnoDB
  DEFAULT CHARSET = utf8mb4
  COLLATE = utf8mb4_unicode_ci;
