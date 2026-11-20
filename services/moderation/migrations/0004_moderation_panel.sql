-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0089 step 5 - what the moderators' panel keeps beside the reports.
--
-- Nothing here names a reporter, and nothing here is published: the public
-- record of a decision is its statement in the moderation log.

-- One case per reported target. Created by the first report of a target;
-- a report arriving after a decision reopens it (status back to new).
-- `status`: 1 new, 2 in review (a moderator holds the lock), 3 decided.
-- The lock is released after 30 minutes without activity by its holder.
CREATE TABLE IF NOT EXISTS moderation_cases (
    target_kind        TINYINT UNSIGNED  NOT NULL COMMENT 'TargetKind byte',
    target             VARBINARY(64)     NOT NULL,
    status             TINYINT UNSIGNED  NOT NULL DEFAULT 1,
    lock_moderator_id  BINARY(16)        NULL,
    lock_at_ms         BIGINT UNSIGNED   NULL COMMENT 'last activity of the lock holder',
    decided_at_ms      BIGINT UNSIGNED   NULL COMMENT 'the last decision',
    PRIMARY KEY (target_kind, target),
    KEY idx_moderation_cases_status (status),
    KEY idx_moderation_cases_lock (lock_at_ms)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- What a moderator's browser verified when it opened a report: how many
-- included messages were checked as written by the target, and how many
-- were not. The service cannot check this itself: the evidence is sealed
-- to the moderators.
CREATE TABLE IF NOT EXISTS moderation_report_checks (
    receipt_id          BINARY(16)         NOT NULL,
    checked_messages    SMALLINT UNSIGNED  NOT NULL,
    unchecked_messages  SMALLINT UNSIGNED  NOT NULL,
    PRIMARY KEY (receipt_id),
    CONSTRAINT fk_moderation_report_checks_report FOREIGN KEY (receipt_id)
        REFERENCES moderation_reports (receipt_id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- The internal note a moderator wrote with a decision, keyed by the
-- decision's id (see moderation_decisions). Never published, never in the
-- log.
CREATE TABLE IF NOT EXISTS moderation_notes (
    decision_id   BINARY(16)       NOT NULL,
    moderator_id  BINARY(16)       NOT NULL,
    note          TEXT             NOT NULL,
    at_ms         BIGINT UNSIGNED  NOT NULL,
    PRIMARY KEY (decision_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Every decision taken in the panel, with the reason in the clear (the log
-- carries only its commitment). `outcome`: the Action byte, or 0 for "no
-- action", which has no log entry. `decision_id` is the logged statement's
-- id, or a random id for no action.
CREATE TABLE IF NOT EXISTS moderation_decisions (
    id            BIGINT UNSIGNED   NOT NULL AUTO_INCREMENT,
    target_kind   TINYINT UNSIGNED  NOT NULL,
    target        VARBINARY(64)     NOT NULL,
    outcome       TINYINT UNSIGNED  NOT NULL,
    reason        TINYINT UNSIGNED  NOT NULL,
    moderator_id  BINARY(16)        NOT NULL,
    decision_id   BINARY(16)        NOT NULL,
    at_ms         BIGINT UNSIGNED   NOT NULL,
    PRIMARY KEY (id),
    KEY idx_moderation_decisions_target (target_kind, target),
    KEY idx_moderation_decisions_at (at_ms),
    KEY idx_moderation_decisions_id (decision_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- The appeals the panel has seen, and how each ended, for the weekly
-- numbers. auth holds the appeal itself; this keeps only when it was sent
-- and its outcome. `outcome`: 0 pending, 1 declined, 2 lifted.
CREATE TABLE IF NOT EXISTS moderation_appeals (
    statement_id   BINARY(16)        NOT NULL,
    target         BINARY(32)        NOT NULL,
    appeal_at_ms   BIGINT UNSIGNED   NOT NULL,
    outcome        TINYINT UNSIGNED  NOT NULL DEFAULT 0,
    outcome_at_ms  BIGINT UNSIGNED   NULL,
    PRIMARY KEY (statement_id, target),
    KEY idx_moderation_appeals_target (target)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
