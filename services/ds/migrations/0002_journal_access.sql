-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors
--
-- ADR-0053: a journal is opened by a key, not by its name.
--
-- A journal with a row here is CLAIMED: the server answers it only to a request
-- carrying a valid proof (crates/lakko-ds-proof). A journal with no row keeps
-- the Phase 1.3 posture (anyone who names it), which is what every journal is
-- until its owner claims it - and what the rendezvous mailboxes (pairing,
-- recovery, courier, state bundle) stay by design.
--
-- The server stores PUBLIC keys only: per group one Ed25519 verifying key per
-- registered epoch (the two newest are kept), per mailbox the identity key its
-- pseudonym is the BLAKE3 of. Neither names a person and neither is key
-- material (invariants #2 and #5).

CREATE TABLE IF NOT EXISTS ds_group_access (
    group_id    BINARY(32)       NOT NULL,
    epoch       BIGINT UNSIGNED  NOT NULL,
    -- Ed25519 verifying key of the group's access key at this epoch.
    access_pk   BINARY(32)       NOT NULL,
    created_at  TIMESTAMP(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (group_id, epoch)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS ds_mailbox_access (
    device_id    BINARY(32)      NOT NULL,
    -- The identity key whose IdentityPseudonym is `device_id`; checked at
    -- claim time, so a row can only ever name the key the mailbox is named
    -- after.
    identity_pk  BINARY(32)      NOT NULL,
    created_at   TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (device_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- A non-member's request to join (ADR-0053 section 4): the same shape as the
-- message journals so the fetch path is shared, in its own table so a knock
-- never lands among the members' rows. Small (16 KiB), PoW-gated, pruned
-- after seven days.
CREATE TABLE IF NOT EXISTS ds_group_knocks (
    id          BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    group_id    BINARY(32)      NOT NULL,
    payload     BLOB            NOT NULL,
    created_at  TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (id),
    INDEX idx_knock_group_id_id (group_id, id),
    INDEX idx_knock_created_at (created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Replay cache: a (journal, nonce) pair is accepted once inside the proof
-- window. In the database rather than in memory so a second DS instance
-- refuses the same replay.
CREATE TABLE IF NOT EXISTS ds_proof_nonces (
    pseudonym   BINARY(32)      NOT NULL,
    nonce       BINARY(16)      NOT NULL,
    seen_at     TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (pseudonym, nonce),
    INDEX idx_nonce_seen_at (seen_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- PoW challenges for the knock - the shape lakko-pow-server expects (the same
-- table services/auth, services/discovery and services/push-gw carry).
CREATE TABLE IF NOT EXISTS pow_challenges (
    seed         BINARY(16)   NOT NULL PRIMARY KEY,
    effort_bits  TINYINT UNSIGNED NOT NULL,
    expires_at   TIMESTAMP(3) NOT NULL,
    INDEX idx_pow_challenges_expires_at (expires_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
