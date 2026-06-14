-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0020 — the independent witness co-signer for KT-log split-view
-- resistance. The witness co-signs an operator root ONLY after verifying a
-- Merkle consistency proof that the new root append-only-extends its
-- last-co-signed root, so the operator cannot get a co-signature on a forked
-- history. Two single-row tables:
--   * witness_key   — the witness's own Ed25519 seed (its trust anchor).
--   * witness_state — its last-co-signed (tree_root, log_size) high-water.

-- ---------------------------------------------------------------------------
-- Witness signing key — the witness's OWN trust anchor (independent of the
-- operator's). Single-row table (`id = 1`); the 32-byte Ed25519 seed is
-- generated on first boot via INSERT IGNORE and the keypair is rebuilt at
-- every start. The derived verifying key is published at GET /witness/key and
-- pinned by clients out-of-band, exactly like the operator key.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS witness_key (
    id    TINYINT UNSIGNED NOT NULL PRIMARY KEY CHECK (id = 1),
    -- 32-byte Ed25519 witness signing seed. NEVER leaves the server.
    seed  BINARY(32)       NOT NULL,
    created_at TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3)
) ENGINE = InnoDB
  DEFAULT CHARSET = utf8mb4
  COLLATE = utf8mb4_unicode_ci;

-- ---------------------------------------------------------------------------
-- Witness last-co-signed high-water — the ONLY log state the witness keeps.
--
-- Single-row table (`id = 1`). The witness verifies each new root's
-- consistency proof against THIS `(tree_root, log_size)` before co-signing,
-- then advances it. It never mirrors the full log — one root + one proof per
-- co-sign is the whole state (the Sigsum / CT-witness model). The row is
-- seeded at the empty tree (`log_size = 0`, all-zero root) on first boot.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS witness_state (
    id        TINYINT UNSIGNED NOT NULL PRIMARY KEY CHECK (id = 1),
    -- The last-co-signed Merkle root (32 bytes). All-zero == empty tree.
    tree_root BINARY(32)       NOT NULL,
    -- The last-co-signed log size in leaves. Monotone — only ever advances.
    log_size  BIGINT UNSIGNED  NOT NULL,
    updated_at TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3)
                                          ON UPDATE CURRENT_TIMESTAMP(3)
) ENGINE = InnoDB
  DEFAULT CHARSET = utf8mb4
  COLLATE = utf8mb4_unicode_ci;
