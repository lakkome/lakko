-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 2 — the persistent CONIKS-style key-transparency log. The
-- `lakko-transparency` crate carries the Merkle-tree + signed-root +
-- inclusion-proof logic; this service wraps it with MariaDB
-- persistence so the log survives a process restart, and signs roots
-- with an operator key generated on first boot.
--
-- Two tables:
--   * transparency_operator_key — the single Ed25519 operator seed.
--   * log_entries               — the append-only leaf rows.

-- ---------------------------------------------------------------------------
-- Operator signing key — the TRUST ANCHOR of the whole log.
--
-- Single-row table (`id = 1`). The 32-byte Ed25519 seed is generated on
-- first boot via `INSERT IGNORE` (mirrors auth's call_jwt_signing_key /
-- opaque_server_setup pattern) and the signing keypair is rebuilt from
-- it at every process start. The PUBLIC half (verifying key) is what
-- clients pin OUT-OF-BAND (shipped in the binary / signed bootstrap
-- config); every SignedRoot this service emits is verified against it.
-- A different key cannot forge a root clients already trust.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS transparency_operator_key (
    id    TINYINT UNSIGNED NOT NULL PRIMARY KEY CHECK (id = 1),
    -- 32-byte Ed25519 operator signing seed. NEVER leaves the server;
    -- only the derived verifying key is published.
    seed  BINARY(32)       NOT NULL,
    -- Bookkeeping — when the operator key was first generated.
    created_at TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3)
) ENGINE = InnoDB
  DEFAULT CHARSET = utf8mb4
  COLLATE = utf8mb4_unicode_ci;

-- ---------------------------------------------------------------------------
-- Append-only log leaves.
--
-- APPEND-ONLY — a row is NEVER updated or deleted once written. Identity
-- rotation appends a NEW row with a higher `version`; it never mutates an
-- existing one. The Merkle tree is rebuilt by replaying these rows in
-- `idx` order at boot, so any UPDATE/DELETE would silently fork the tree
-- from what clients have already proven inclusion against.
--
-- `idx` is the InMemoryLog 0-based append index, supplied by the
-- application (NOT auto-increment) so the DB row stays in lockstep with
-- the in-memory Merkle leaf position. The app appends in-memory (which
-- returns the index), then INSERTs the row with exactly that index.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS log_entries (
    -- The InMemoryLog 0-based append index — NOT auto-increment, the app
    -- supplies it to stay in lockstep with the in-memory tree.
    idx             BIGINT UNSIGNED NOT NULL PRIMARY KEY,
    -- BLAKE3 hash of the canonical username form (32 bytes).
    username_hash   BINARY(32)      NOT NULL,
    -- Identity public key (Ed25519 — 32 bytes; VARBINARY(64) leaves room
    -- for an X.509-cert-bound key without a schema change).
    identity_pubkey VARBINARY(64)   NOT NULL,
    -- Monotonic per-username version; >= 1, incremented per rotation.
    version         BIGINT UNSIGNED NOT NULL,
    -- ADR-0017 fresh-start marker: a no-supersession-proof owner reset
    -- (the "lost everything" publish). Carried so a client / auditor can
    -- distinguish a proven rotation from a fresh start when walking the log.
    is_reset        BOOLEAN         NOT NULL DEFAULT FALSE,
    -- When the leaf was appended.
    created_at      TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    -- Per-username history lookup: all versions for a username_hash.
    INDEX idx_username (username_hash, version)
) ENGINE = InnoDB
  DEFAULT CHARSET = utf8mb4
  COLLATE = utf8mb4_unicode_ci;
