-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0089 step 5: a channel is moderated only by a moderator's signed
-- statement, forwarded by the moderation service and checked here against
-- the moderator list under the pinned moderation root. The ADR-0059
-- shared-secret route and its remove / block / unblock actions are gone.
--
-- 1. `channel_moderation` keeps one row per moderated channel and gains:
--    `delisted` (ChannelDelist: out of Discover, a new listing refused with
--    `403 listing-delisted`), `removed` (ChannelRemove: every channel route
--    answers `410 channel-removed`), the statement's `log_index` (served
--    with the 410) and `issued_at_ms` (the newest statement applied; an
--    older one changes nothing). `blocked` is ChannelBlockHandle (the
--    ADR-0059 block: delisted and a new listing refused with
--    `403 listing-blocked`). ChannelRestore clears all three. `reason`
--    holds the reason's stable name. No production data exists; existing
--    rows keep `issued_at_ms` 0.
--
-- 2. `moderation_applied_statements`, `moderation_list_state`: the statement
--    ids applied (a replay is a repeat) and the highest moderator-list
--    version seen (an older one is refused).

ALTER TABLE channel_moderation
    ADD COLUMN delisted     TINYINT(1)      NOT NULL DEFAULT 0,
    ADD COLUMN removed      TINYINT(1)      NOT NULL DEFAULT 0,
    ADD COLUMN log_index    BIGINT UNSIGNED NULL     COMMENT 'the newest applied statement in the moderation log',
    ADD COLUMN issued_at_ms BIGINT UNSIGNED NOT NULL DEFAULT 0
        COMMENT 'issued_at_ms of the newest statement applied';

CREATE TABLE IF NOT EXISTS moderation_applied_statements (
    statement_id  BINARY(16)      NOT NULL,
    applied_at_ms BIGINT UNSIGNED NOT NULL COMMENT 'unix ms channels applied it',
    PRIMARY KEY (statement_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS moderation_list_state (
    id           TINYINT UNSIGNED NOT NULL,
    list_version BIGINT UNSIGNED  NOT NULL COMMENT 'the highest moderator-list version seen',
    PRIMARY KEY (id),
    CONSTRAINT ck_moderation_list_state_one_row CHECK (id = 1)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
