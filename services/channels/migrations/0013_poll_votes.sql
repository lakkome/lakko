-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0092 section 4.3: channel poll votes.
--
-- A vote is a row per `(post, tag)`. The tag is the per-poll tag of an
-- anonymous credential presentation (`lakko-anon-cred`): the same for one
-- account and one poll, unlinkable across polls and to the account. The
-- service never learns which account voted, which option a label is, the
-- question, the option count or the close time.
--
-- - `channel_poll_votes`: the row. `labels` is the concatenation of 1..=10
--   sorted 16-byte opaque option labels; empty after a retract (a tombstone,
--   so `at_ms` still orders later requests and a captured older request
--   cannot restore the vote). `at_ms` is the client time bound into the
--   presentation; a request is accepted only when strictly newer.
-- - `channel_poll_vote_counts`: votes per label, maintained in the
--   transaction that writes the row.
-- - `channel_poll_voter_counts`: tags with a counted vote, the same way.
--
-- All three cascade from `channel_posts`, so a post pruned after its 30 days
-- (or a deleted channel) takes its votes with it.

CREATE TABLE IF NOT EXISTS channel_poll_votes (
    channel_id     BINARY(16)      NOT NULL,
    post_cursor    BIGINT UNSIGNED NOT NULL,
    tag            BINARY(32)      NOT NULL
                     COMMENT 'credential tag s*H_poll; links no account',
    labels         VARBINARY(160)  NOT NULL
                     COMMENT 'sorted 16-byte option labels; empty after a retract',
    at_ms          BIGINT UNSIGNED NOT NULL
                     COMMENT 'client time bound into the presentation; strictly increasing per tag',
    PRIMARY KEY (channel_id, post_cursor, tag),
    CONSTRAINT fk_channel_poll_votes_post
        FOREIGN KEY (channel_id, post_cursor) REFERENCES channel_posts (channel_id, `cursor`)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS channel_poll_vote_counts (
    channel_id     BINARY(16)      NOT NULL,
    post_cursor    BIGINT UNSIGNED NOT NULL,
    label          BINARY(16)      NOT NULL,
    votes          BIGINT UNSIGNED NOT NULL DEFAULT 0,
    PRIMARY KEY (channel_id, post_cursor, label),
    CONSTRAINT fk_channel_poll_vote_counts_post
        FOREIGN KEY (channel_id, post_cursor) REFERENCES channel_posts (channel_id, `cursor`)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS channel_poll_voter_counts (
    channel_id     BINARY(16)      NOT NULL,
    post_cursor    BIGINT UNSIGNED NOT NULL,
    voters         BIGINT UNSIGNED NOT NULL DEFAULT 0,
    PRIMARY KEY (channel_id, post_cursor),
    CONSTRAINT fk_channel_poll_voter_counts_post
        FOREIGN KEY (channel_id, post_cursor) REFERENCES channel_posts (channel_id, `cursor`)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
