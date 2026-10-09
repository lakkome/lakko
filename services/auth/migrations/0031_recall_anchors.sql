-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors
--
-- ADR-0066 sprint T3 — the recall (taking the account back from an old device
-- after a reset): section 0.2 (recall anchors) and section 1.2 (the routes).
--
-- `recall_anchors` is what the account was BEFORE the first change of the
-- password or of the identity key made while a given identity key was
-- current: that identity public key (the key that may recall) and the OPAQUE
-- password registration then in force (what the recall's password is checked
-- against). One row per (account, identity key), never overwritten: the
-- UNIQUE key is the backstop of the capture rule, which runs under the
-- account row's lock. Rows are ordered by `seq`; a lower `seq` is an OLDER
-- standing and outranks a higher one (a recall through anchor i deletes i and
-- every higher one and keeps the lower ones).
--
-- What is stored: a public key discovery already publishes for the account,
-- and a password VERIFIER of the same trust class as the live envelope in
-- `opaque_registrations` (invariant #2: no key material; invariant #1: no
-- PII). Kept for the recall window (`LAKKO_RECALL_WINDOW_DAYS`, default 30)
-- and pruned hourly after it.
--
-- `attempts` counts the password attempts opened against the anchor (one per
-- accepted `POST /account/recall/start`: with OPAQUE the start's answer is
-- what lets a client test one password guess, so the start is the step that
-- is counted). At the cap the anchor is destroyed.

CREATE TABLE IF NOT EXISTS recall_anchors (
    account_id            BINARY(16)      NOT NULL COMMENT 'FK -> accounts.id',
    seq                   BIGINT UNSIGNED NOT NULL COMMENT 'per account, increasing; lower = older standing',
    identity_pubkey       BINARY(32)      NOT NULL COMMENT 'the identity key that may recall',
    password_registration VARBINARY(4096) NOT NULL COMMENT 'OPAQUE password registration in force before the first change',
    key_generation        BIGINT UNSIGNED NOT NULL COMMENT 'accounts.identity_version when captured',
    created_at_ms         BIGINT          NOT NULL COMMENT 'unix ms of the capture (the first change)',
    expires_at_ms         BIGINT          NOT NULL COMMENT 'unix ms; the anchor is dead at or after it',
    attempts              INT UNSIGNED    NOT NULL DEFAULT 0 COMMENT 'password attempts opened; destroyed at the cap',
    PRIMARY KEY (account_id, seq),
    UNIQUE KEY uq_recall_anchors_key (account_id, identity_pubkey),
    CONSTRAINT fk_recall_anchors_account
        FOREIGN KEY (account_id) REFERENCES accounts (id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE INDEX idx_recall_anchors_expires
    ON recall_anchors (expires_at_ms);

-- The in-flight state of one recall attempt, between `recall/start` and
-- `recall/finish`: the OPAQUE `ServerLogin` state against the anchored
-- password, the anchor it was opened for, and the 32-byte server nonce the
-- finish's account statement must cover. Single-use (the finish deletes it),
-- short-lived, and deleted with every other in-flight state when the account's
-- keys change. Its own table, not `opaque_login_states`: no login route can
-- consume one, and the nonce and the anchor have columns.

CREATE TABLE IF NOT EXISTS recall_states (
    session_id    BINARY(16)      NOT NULL PRIMARY KEY,
    account_id    BINARY(16)      NOT NULL COMMENT 'FK -> accounts.id',
    anchor_seq    BIGINT UNSIGNED NOT NULL,
    server_nonce  BINARY(32)      NOT NULL,
    state_bytes   VARBINARY(2048) NOT NULL COMMENT 'serialized ServerLogin',
    created_at_ms BIGINT          NOT NULL,
    INDEX idx_recall_states_account (account_id),
    INDEX idx_recall_states_created (created_at_ms),
    CONSTRAINT fk_recall_states_account
        FOREIGN KEY (account_id) REFERENCES accounts (id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- A key reset and a recall delete an account's in-flight login states inside
-- their transaction. `opaque_login_states` had no index on `account_id`, so
-- that DELETE scanned and locked every row of the table, every other
-- account's in-flight login included, until the transaction committed.

CREATE INDEX idx_opaque_login_states_account
    ON opaque_login_states (account_id);
