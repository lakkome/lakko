-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0089 section 2 - the reports Lakko's moderators review.
--
-- A row is one anonymous report: the clear part (what is reported, why,
-- where it was seen, how many pieces of evidence) and the whole report as
-- received, whose content is sealed to the moderator list and never opened
-- here. Nothing names the reporter: no account, no address, no token, and
-- only the day of arrival, not the time.
--
-- `receipt_id` is random; the reporter's device keeps it to read the
-- status. `status`: 1 received, 2 action taken, 3 no action.
CREATE TABLE IF NOT EXISTS moderation_reports (
    receipt_id        BINARY(16)        NOT NULL,
    target_kind       TINYINT UNSIGNED  NOT NULL COMMENT 'TargetKind byte',
    target            VARBINARY(64)     NOT NULL,
    reason            TINYINT UNSIGNED  NOT NULL COMMENT 'Reason byte',
    conversation_kind TINYINT UNSIGNED  NOT NULL COMMENT 'ConversationKind byte',
    evidence_count    TINYINT UNSIGNED  NOT NULL,
    list_version      BIGINT UNSIGNED   NOT NULL COMMENT 'moderator list sealed to',
    report            MEDIUMBLOB        NOT NULL COMMENT 'the report, content sealed',
    received_day      INT UNSIGNED      NOT NULL COMMENT 'days since the unix epoch',
    status            TINYINT UNSIGNED  NOT NULL DEFAULT 1,
    PRIMARY KEY (receipt_id),
    KEY idx_moderation_reports_target (target_kind, target),
    KEY idx_moderation_reports_status (status, received_day)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
